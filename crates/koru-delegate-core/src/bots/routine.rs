// SPDX-License-Identifier: MPL-2.0
//! Stored Routine provenance plus digest values shared by Connections.
use super::v4;
use crate::{HandoffId, RunOrigin, ScheduleId};
use serde::{Deserialize, Serialize};
mod digest;
pub use digest::{RoutineDigestError, routine_digest_json};

/// Native Routine control observations. These values carry no delegate command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineControlOutcome {
    Applied,
    AlreadyApplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineControlLifecycle {
    Active,
    Paused,
}

/// Closure observation retained by native Routine detail projections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCloseReason {
    ExpiredUnclaimed,
    SlotSkipped,
    OtherWinner,
    ConfigurationChanged,
    AuthorityChanged,
    Stopped,
}

/// A fixed digest with one wire spelling. Hash computation belongs to the
/// owner of the versioned semantic encoding: the neutral policy owner here,
/// Schedule owner, or private AI preparation owner. A digest grants no authority.
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
