// SPDX-License-Identifier: MPL-2.0
//! Compact seed transport, matching the existing creation contract's hex
//! representation instead of expanding every byte into a JSON integer.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}
fn decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || text.len() > super::MAX_RECORD_BYTES {
        return None;
    }
    let nibble = |b| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| Some(nibble(b[0])? * 16 + nibble(b[1])?))
        .collect()
}
pub(super) fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    encode(value).serialize(serializer)
}
pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    decode(&String::deserialize(deserializer)?).ok_or_else(|| D::Error::custom("invalid_seed"))
}
pub(super) mod optional {
    use super::*;
    pub(crate) fn serialize<S: Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|bytes| encode(bytes))
            .serialize(serializer)
    }
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| decode(&text).ok_or_else(|| D::Error::custom("invalid_seed")))
            .transpose()
    }
}
