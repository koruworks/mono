// SPDX-License-Identifier: MPL-2.0
//! Memory preserves ordinary document node UUIDs, including genesis v5 IDs.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BotMemoryNodeId(Uuid);

impl BotMemoryNodeId {
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
    pub fn to_storage(&self) -> String {
        self.0.to_string()
    }
}
impl fmt::Display for BotMemoryNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for BotMemoryNodeId {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = Uuid::parse_str(value).map_err(|_| "invalid Memory node UUID")?;
        if id.to_string() != value {
            return Err("noncanonical Memory node UUID");
        }
        Ok(Self(id))
    }
}
impl Serialize for BotMemoryNodeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_storage())
    }
}
impl<'de> Deserialize<'de> for BotMemoryNodeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
