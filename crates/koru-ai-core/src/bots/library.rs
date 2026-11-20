// SPDX-License-Identifier: MPL-2.0

//! Durable Library references, never document content or Store authority.
use super::*;
use std::collections::BTreeSet;

pub const BOT_KNOWLEDGE_LIMIT: usize = 16;

/// The owner's bounded Library settings. References are context for new tasks,
/// not edit grants. `memory` names only a bound page; `None` makes no claim
/// about an unfinished creation and exposes no creation machinery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotLibraryView {
    pub bot_id: BotId,
    pub revision: u64,
    pub role: BotPage,
    pub knowledge: Vec<BotPage>,
    pub memory: Option<BotPage>,
}

/// Immutable acknowledgement of one Knowledge save. Reload Library for current
/// settings; an exact retry returns this original revision and ordered pages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotKnowledgeReceipt {
    pub bot_id: BotId,
    pub request_id: BotRequestId,
    pub revision: u64,
    pub pages: Vec<BotPage>,
}

pub(super) fn valid_id(id: uuid::Uuid) -> bool {
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

#[cfg(test)]
mod tests;
