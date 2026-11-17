// SPDX-License-Identifier: MPL-2.0

//! An opaque JSON object that keeps its bytes.
//!
//! A receipt's `response` is "the existing public response JSON" of whichever
//! local command ran. The owner stores it and replays it; a client reads it as
//! the type it knows the command answers with. Neither end may reorder it:
//! `serde_json::Value` keeps a map's order only when a feature some other
//! crate turns on is unified into the build, so the same value would have two
//! byte forms. [`JsonObject`] keeps the members in the order they were
//! written, in every build.

use std::fmt;

use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::frame::WireError;

/// One JSON object, members in the order they were written. Its `Debug`
/// prints only how many members it has.
#[derive(Clone, PartialEq)]
pub struct JsonObject(Vec<(String, Json)>);

#[derive(Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl JsonObject {
    /// The JSON of `value`, which must serialize as an object.
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> Result<Self, WireError> {
        let bytes = serde_json::to_vec(value).map_err(|_| WireError::Malformed)?;
        serde_json::from_slice(&bytes).map_err(|_| WireError::Malformed)
    }

    /// Read the object as `T`.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, WireError> {
        let bytes = serde_json::to_vec(self).map_err(|_| WireError::Malformed)?;
        serde_json::from_slice(&bytes).map_err(|_| WireError::Malformed)
    }

    /// The object's JSON text, as it goes on the wire.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a JSON object serializes")
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for JsonObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "JsonObject({} members)", self.0.len())
    }
}

fn serialize_members<S: Serializer>(
    members: &[(String, Json)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(members.len()))?;
    for (key, value) in members {
        map.serialize_entry(key, value)?;
    }
    map.end()
}

impl Serialize for JsonObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_members(&self.0, serializer)
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Unsigned(value) => serializer.serialize_u64(*value),
            Self::Signed(value) => serializer.serialize_i64(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(value)?;
                }
                seq.end()
            }
            Self::Object(members) => serialize_members(members, serializer),
        }
    }
}

fn read_members<'de, A: MapAccess<'de>>(mut map: A) -> Result<Vec<(String, Json)>, A::Error> {
    let mut members = Vec::with_capacity(map.size_hint().unwrap_or(0).min(64));
    while let Some(entry) = map.next_entry::<String, Json>()? {
        members.push(entry);
    }
    Ok(members)
}

impl<'de> Deserialize<'de> for JsonObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = JsonObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<JsonObject, A::Error> {
                read_members(map).map(JsonObject)
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Json;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }

            fn visit_none<E: de::Error>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }

            fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
                Json::deserialize(deserializer)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Json, E> {
                Ok(Json::Bool(value))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Json, E> {
                Ok(Json::Unsigned(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Json, E> {
                Ok(match u64::try_from(value) {
                    Ok(value) => Json::Unsigned(value),
                    Err(_) => Json::Signed(value),
                })
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Json, E> {
                Ok(Json::Float(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
                Ok(Json::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Json, E> {
                Ok(Json::String(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(64));
                while let Some(value) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Json::Array(values))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Json, A::Error> {
                read_members(map).map(Json::Object)
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}
