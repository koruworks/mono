// SPDX-License-Identifier: MPL-2.0

//! Routine identities are retained evidence, never claim or execution authority.

use super::{BotOwner, BotSubmission, v4};
use crate::{BotId, BotRequestId, HandoffId, RunId, RunOrigin, ScheduleId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const INTERACTIVE_BOT_SUBMISSION_VERSION: u32 = 1;
pub const ROUTINE_BOT_SUBMISSION_VERSION: u32 = 2;

/// A fixed digest with one wire spelling. Hash computation belongs to the
/// owner of the versioned semantic encoding, not this vocabulary crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RoutineDigest32([u8; 32]);

#[cfg(feature = "schema")]
impl schemars::JsonSchema for RoutineDigest32 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RoutineDigest32".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "pattern":"^[0-9a-f]{64}$"})
    }
}

impl RoutineDigest32 {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl TryFrom<String> for RoutineDigest32 {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid Routine digest");
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| "invalid Routine digest")?;
        }
        Ok(Self(bytes))
    }
}
impl From<RoutineDigest32> for String {
    fn from(value: RoutineDigest32) -> Self {
        use std::fmt::Write;
        let mut text = String::with_capacity(64);
        for byte in value.0 {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineSlot {
    pub owner: BotOwner,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub occurrence_ms: i64,
}
impl RoutineSlot {
    pub fn valid(&self) -> bool {
        crate::DeviceId::parse(&self.owner.device.to_storage()) == Some(self.owner.device)
            && v4(self.bot.as_uuid())
            && v4(self.schedule.as_uuid())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineDefinitionIdentity {
    pub revision: u32,
    pub digest: RoutineDigest32,
    pub owner_handoff: Option<HandoffId>,
}
impl RoutineDefinitionIdentity {
    pub fn valid(&self) -> bool {
        self.revision > 0 && self.owner_handoff.is_none_or(|id| v4(id.as_uuid()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineProvenance {
    pub schedule: ScheduleId,
    pub occurrence_ms: i64,
    pub definition: RoutineDefinitionIdentity,
    pub grant_revision: u64,
}
impl RoutineProvenance {
    pub fn valid(&self) -> bool {
        v4(self.schedule.as_uuid())
            && self.definition.valid()
            && self.grant_revision > 0
            && self.grant_revision <= i64::MAX as u64
    }
    pub fn matches_origin(&self, origin: &RunOrigin) -> bool {
        self.valid()
            && matches!(origin, RunOrigin::Scheduled { schedule, occurrence_ms, revision }
            if *schedule == self.schedule && *occurrence_ms == self.occurrence_ms && *revision == self.definition.revision)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RoutineExecutionIds {
    pub submission: BotRequestId,
    pub run: RunId,
    pub chat: Uuid,
    pub attempt: Uuid,
    pub user_message: Uuid,
    pub assistant_message: Uuid,
}
impl RoutineExecutionIds {
    pub fn valid(&self) -> bool {
        let ids = [
            self.submission.as_uuid(),
            self.run.as_uuid(),
            self.chat,
            self.attempt,
            self.user_message,
            self.assistant_message,
        ];
        ids.iter().all(|id| v4(*id))
            && ids
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == ids.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutinePrepared {
    pub slot: RoutineSlot,
    pub definition: RoutineDefinitionIdentity,
    pub ids: RoutineExecutionIds,
    pub semantic_hash: RoutineDigest32,
    pub preparation_hash: RoutineDigest32,
    pub submission_version: u8,
    pub submission: BotSubmission,
}
impl RoutinePrepared {
    pub fn valid(&self) -> bool {
        let p = &self.submission.provenance;
        self.slot.valid()
            && self.definition.valid()
            && self.ids.valid()
            && u32::from(self.submission_version) == ROUTINE_BOT_SUBMISSION_VERSION
            && self
                .submission
                .valid_for_version(u32::from(self.submission_version))
            && p.owner == self.slot.owner
            && p.bot == self.slot.bot
            && p.submission == self.ids.submission
            && self.submission.request.run == self.ids.run
            && p.chat_id == self.ids.chat
            && p.ai_attempt_id == Some(self.ids.attempt)
            && p.assistant_message_id == Some(self.ids.assistant_message)
            && p.routine.is_some_and(|r| {
                r.schedule == self.slot.schedule
                    && r.occurrence_ms == self.slot.occurrence_ms
                    && r.definition == self.definition
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineClaimIdentity {
    pub slot: RoutineSlot,
    pub definition: RoutineDefinitionIdentity,
    pub ids: RoutineExecutionIds,
    pub preparation_hash: RoutineDigest32,
    pub claimed_at_ms: i64,
}
impl RoutineClaimIdentity {
    pub fn valid(&self) -> bool {
        self.slot.valid() && self.definition.valid() && self.ids.valid()
    }
    pub fn matches_prepared(&self, prepared: &RoutinePrepared) -> bool {
        self.valid()
            && prepared.valid()
            && self.slot == prepared.slot
            && self.definition == prepared.definition
            && self.ids == prepared.ids
            && self.preparation_hash == prepared.preparation_hash
    }
}

#[cfg(test)]
mod tests;
