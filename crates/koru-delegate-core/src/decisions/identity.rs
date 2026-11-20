// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::RunId;
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

/// Shared digest spelling used by standalone Connections.
pub type DecisionDigest = crate::RoutineDigest32;
