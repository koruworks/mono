// SPDX-License-Identifier: MPL-2.0

//! Native policy epoch correlation, distinct from a public Bot command ID.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr};
use uuid::{Uuid, Variant};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativePolicyRequestId(Uuid);

impl NativePolicyRequestId {
    pub fn to_storage(self) -> String {
        self.0.hyphenated().to_string()
    }
}
impl From<crate::BotRequestId> for NativePolicyRequestId {
    fn from(value: crate::BotRequestId) -> Self {
        Self(value.as_uuid())
    }
}
impl TryFrom<Uuid> for NativePolicyRequestId {
    type Error = &'static str;
    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        if value.is_nil()
            || value.get_variant() != Variant::RFC4122
            || !matches!(value.get_version_num(), 4 | 8)
        {
            return Err("expected a non-nil RFC UUID version 4 or 8");
        }
        Ok(Self(value))
    }
}
impl FromStr for NativePolicyRequestId {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = Uuid::parse_str(value).map_err(|_| "invalid policy UUID")?;
        if id.hyphenated().to_string() != value {
            return Err("expected a canonical lowercase hyphenated UUID");
        }
        id.try_into()
    }
}
impl fmt::Debug for NativePolicyRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NativePolicy({})", &self.to_storage()[..8])
    }
}
impl Serialize for NativePolicyRequestId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_storage())
    }
}
impl<'de> Deserialize<'de> for NativePolicyRequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
