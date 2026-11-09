// SPDX-License-Identifier: MPL-2.0

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Exact version marker for v1 security and authentication messages.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Version1;

impl fmt::Debug for Version1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Version1")
    }
}

impl Serialize for Version1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(1)
    }
}

impl<'de> Deserialize<'de> for Version1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u64::deserialize(deserializer)?;
        if version == 1 {
            Ok(Self)
        } else {
            Err(de::Error::custom(format!(
                "unsupported account message version {version}"
            )))
        }
    }
}

impl JsonSchema for Version1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AccountWireVersion1")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({ "type": "integer", "const": 1 })
    }
}

/// Exact version marker for the v2 Cloud catalog messages
/// ([`crate::root_catalog`]). A v1 reader refuses it on the version alone,
/// which is the whole point: the v2 shapes live beside the v1 ones and
/// never change a v1 meaning.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Version2;

impl fmt::Debug for Version2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Version2")
    }
}

impl Serialize for Version2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(2)
    }
}

impl<'de> Deserialize<'de> for Version2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u64::deserialize(deserializer)?;
        if version == 2 {
            Ok(Self)
        } else {
            Err(de::Error::custom(format!(
                "unsupported account message version {version}"
            )))
        }
    }
}

impl JsonSchema for Version2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AccountWireVersion2")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({ "type": "integer", "const": 2 })
    }
}
