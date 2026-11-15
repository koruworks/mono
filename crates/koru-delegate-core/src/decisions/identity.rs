// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{AgentTurnId, AttemptGeneration, BotId, BotOwner, RunAttemptId, RunId, SessionId};
use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! decision_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(RunId);
        impl $name {
            pub fn mint() -> Self {
                Self(RunId::mint())
            }
            pub fn as_uuid(self) -> Uuid {
                self.0.as_uuid()
            }
            pub fn to_storage(self) -> String {
                self.0.to_storage()
            }
        }
        impl FromStr for $name {
            type Err = crate::IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse().map(Self)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), &self.to_storage()[..8])
            }
        }
        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
                <RunId as schemars::JsonSchema>::json_schema(g)
            }
        }
    };
}
decision_id!(DecisionId);
decision_id!(DecisionAnswerId);
decision_id!(BotWaitId);
decision_id!(DecisionTransitionId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct DecisionRevision(u64);
impl DecisionRevision {
    pub fn new(value: u64) -> Result<Self, DecisionError> {
        require(value > 0 && value <= i64::MAX as u64)?;
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl TryFrom<u64> for DecisionRevision {
    type Error = DecisionError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<DecisionRevision> for u64 {
    fn from(value: DecisionRevision) -> Self {
        value.0
    }
}
#[cfg(feature = "schema")]
impl schemars::JsonSchema for DecisionRevision {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DecisionRevision".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"integer","minimum":1,"maximum":i64::MAX})
    }
}

/// Explicit version marker: no unknown future version is accepted as V1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct DecisionVersionV1;
impl TryFrom<u8> for DecisionVersionV1 {
    type Error = DecisionError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        require(v == 1)?;
        Ok(Self)
    }
}
impl From<DecisionVersionV1> for u8 {
    fn from(_: DecisionVersionV1) -> Self {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionFence {
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    pub attempt: RunAttemptId,
    #[serde(deserialize_with = "generation")]
    pub generation: AttemptGeneration,
    pub session: SessionId,
    pub turn: AgentTurnId,
}
impl DecisionValidate for DecisionFence {
    fn validate(&self) -> Result<(), DecisionError> {
        require(
            crate::DeviceId::parse(&self.owner.device.to_storage()) == Some(self.owner.device)
                && self.generation.get() > 0
                && self.bot.to_storage().parse::<BotId>() == Ok(self.bot)
                && self.run.to_storage().parse::<RunId>() == Ok(self.run)
                && self.attempt.to_storage().parse::<RunAttemptId>() == Ok(self.attempt),
        )
    }
}
pub(super) fn generation<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<AttemptGeneration, D::Error> {
    AttemptGeneration::from_raw(u32::deserialize(d)?)
        .ok_or_else(|| serde::de::Error::custom(DecisionError::Invalid))
}
pub(super) fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

/// Digest bytes reuse the reviewed canonical lowercase-hex owner.
pub type DecisionDigest = crate::RoutineDigest32;
