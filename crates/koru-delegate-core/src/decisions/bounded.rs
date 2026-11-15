// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde::{
    Deserializer, Serializer,
    de::{SeqAccess, Visitor},
};
use std::{fmt, io::Write};

/// Nonempty opaque bytes. Length is bytes, not Unicode characters. Serialization
/// is a JSON byte array; the surrounding frame has a separate encoded-size cap.
#[derive(Clone, PartialEq, Eq)]
pub struct DecisionBytes<const MAX: usize>(Vec<u8>);
impl<const MAX: usize> DecisionBytes<MAX> {
    pub fn new(bytes: Vec<u8>) -> Result<Self, DecisionError> {
        if bytes.len() > MAX {
            return Err(DecisionError::TooLarge);
        }
        require(!bytes.is_empty())?;
        Ok(Self(bytes))
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}
impl<const MAX: usize> fmt::Debug for DecisionBytes<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DecisionBytes([REDACTED])")
    }
}
impl<const MAX: usize> Serialize for DecisionBytes<MAX> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
impl<'de, const MAX: usize> Deserialize<'de> for DecisionBytes<MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct BytesVisitor<const MAX: usize>;
        impl<'de, const MAX: usize> Visitor<'de> for BytesVisitor<MAX> {
            type Value = DecisionBytes<MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded nonempty bytes")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq.size_hint().is_some_and(|n| n > MAX) {
                    return Err(serde::de::Error::custom(DecisionError::TooLarge));
                }
                let mut bytes = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    if bytes.len() == MAX {
                        return Err(serde::de::Error::custom(DecisionError::TooLarge));
                    }
                    bytes.push(byte);
                }
                DecisionBytes::new(bytes).map_err(serde::de::Error::custom)
            }
        }
        d.deserialize_seq(BytesVisitor::<MAX>)
    }
}
#[cfg(feature = "schema")]
impl<const MAX: usize> schemars::JsonSchema for DecisionBytes<MAX> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("DecisionBytes{MAX}").into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"array","minItems":1,"maxItems":MAX,"items":{"type":"integer","minimum":0,"maximum":255}})
    }
}
pub type DecisionPayload = DecisionBytes<DECISION_PAYLOAD_BYTES>;
pub type DecisionVersionVector = DecisionBytes<65_536>;
pub type DecisionPreparedDelta = DecisionBytes<DECISION_DELTA_BYTES>;

pub(super) fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, DecisionError> {
    struct BoundedWriter {
        bytes: Vec<u8>,
        remaining: usize,
        exceeded: bool,
    }
    impl Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
                self.exceeded = true;
                return Err(std::io::Error::other("decision record too large"));
            };
            self.bytes.extend_from_slice(bytes);
            self.remaining = remaining;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        remaining: limit,
        exceeded: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        if writer.exceeded {
            DecisionError::TooLarge
        } else {
            DecisionError::Invalid
        }
    })?;
    Ok(writer.bytes)
}

/// A bounded sequence without trusting a deserializer's allocation hint.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DecisionList<T, const MAX: usize>(Vec<T>);
impl<T, const MAX: usize> Default for DecisionList<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T, const MAX: usize> DecisionList<T, MAX> {
    pub fn new(items: Vec<T>) -> Result<Self, DecisionError> {
        if items.len() > MAX {
            return Err(DecisionError::TooLarge);
        }
        Ok(Self(items))
    }
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}
impl<T, const MAX: usize> fmt::Debug for DecisionList<T, MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DecisionList([REDACTED])")
    }
}
impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for DecisionList<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ListVisitor<T, const MAX: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for ListVisitor<T, MAX> {
            type Value = DecisionList<T, MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded decision list")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq.size_hint().is_some_and(|n| n > MAX) {
                    return Err(serde::de::Error::custom(DecisionError::TooLarge));
                }
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    if items.len() == MAX {
                        return Err(serde::de::Error::custom(DecisionError::TooLarge));
                    }
                    items.push(item);
                }
                Ok(DecisionList(items))
            }
        }
        d.deserialize_seq(ListVisitor::<T, MAX>(std::marker::PhantomData))
    }
}
