// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde::de::DeserializeOwned;
use std::io::Write;

/// Content-free codec failures. None of these variants proves absence of a
/// preparation/claim; transport owners must preserve unknown completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutineBridgeCodecError {
    Invalid,
    ContextTooLarge,
    Mismatch,
}

pub(super) mod sealed {
    pub trait Sealed {}
}

/// Only the private bridge's closed DTOs implement this bound. Validation is
/// shape validation; it never supplies Store, policy or delegate authority.
pub trait RoutineBridgePayload: sealed::Sealed {
    fn valid(&self) -> bool;
}

pub trait RoutineBridgeReply<Request>: RoutineBridgePayload {
    fn matches_request(&self, request: &Request) -> bool;
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineBridgeEnvelope<T> {
    #[serde(deserialize_with = "version_one")]
    pub version: u8,
    #[serde(deserialize_with = "request_uuid")]
    pub request_id: Uuid,
    pub payload: T,
}

impl<T> std::fmt::Debug for RoutineBridgeEnvelope<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineBridgeEnvelope")
            .field("version", &self.version)
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl<T: RoutineBridgePayload> RoutineBridgeEnvelope<T> {
    pub fn new(payload: T) -> Result<Self, RoutineBridgeCodecError> {
        let envelope = Self {
            version: 1,
            request_id: Uuid::new_v4(),
            payload,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn reply_to<R: RoutineBridgePayload>(
        request: &RoutineBridgeEnvelope<R>,
        payload: T,
    ) -> Result<Self, RoutineBridgeCodecError>
    where
        T: RoutineBridgeReply<R>,
    {
        request.validate()?;
        let reply = Self {
            version: 1,
            request_id: request.request_id,
            payload,
        };
        reply.validate_reply_to(request)?;
        Ok(reply)
    }

    pub fn validate(&self) -> Result<(), RoutineBridgeCodecError> {
        if self.version != 1 || !v4(self.request_id) || !self.payload.valid() {
            return Err(RoutineBridgeCodecError::Invalid);
        }
        Ok(())
    }

    pub fn validate_reply_to<R: RoutineBridgePayload>(
        &self,
        request: &RoutineBridgeEnvelope<R>,
    ) -> Result<(), RoutineBridgeCodecError>
    where
        T: RoutineBridgeReply<R>,
    {
        self.validate()?;
        request.validate()?;
        if self.request_id != request.request_id || !self.payload.matches_request(&request.payload)
        {
            return Err(RoutineBridgeCodecError::Mismatch);
        }
        Ok(())
    }
}

struct Count {
    remaining: usize,
    written: usize,
    exceeded: bool,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("Routine envelope too large"));
        };
        self.remaining = remaining;
        self.written += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<T: RoutineBridgePayload + Serialize> RoutineBridgeEnvelope<T> {
    /// Count the complete encoded envelope before allocating its body. All
    /// admitted payloads are closed, immutable DTOs; neither pass truncates.
    pub fn encode(&self) -> Result<Vec<u8>, RoutineBridgeCodecError> {
        self.validate()?;
        let mut count = Count {
            remaining: ROUTINE_BRIDGE_BODY_LIMIT,
            written: 0,
            exceeded: false,
        };
        if serde_json::to_writer(&mut count, self).is_err() {
            return Err(if count.exceeded {
                RoutineBridgeCodecError::ContextTooLarge
            } else {
                RoutineBridgeCodecError::Invalid
            });
        }
        let mut bytes = Vec::with_capacity(count.written);
        serde_json::to_writer(&mut bytes, self).map_err(|_| RoutineBridgeCodecError::Invalid)?;
        Ok(bytes)
    }
}

impl<T: RoutineBridgePayload + DeserializeOwned> RoutineBridgeEnvelope<T> {
    /// The later HTTP owner must enforce the same limit while streaming. This
    /// slice codec rejects oversize bodies before deserializing any DTO fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, RoutineBridgeCodecError> {
        if bytes.len() > ROUTINE_BRIDGE_BODY_LIMIT {
            return Err(RoutineBridgeCodecError::ContextTooLarge);
        }
        let envelope: Self =
            serde_json::from_slice(bytes).map_err(|_| RoutineBridgeCodecError::Invalid)?;
        envelope.validate()?;
        Ok(envelope)
    }
}

fn version_one<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let version = u8::deserialize(d)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(
            "unsupported Routine bridge version",
        ))
    }
}
fn request_uuid<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(d)?;
    let id = Uuid::parse_str(&text)
        .map_err(|_| serde::de::Error::custom("invalid Routine request UUID"))?;
    if !v4(id) || id.to_string() != text {
        return Err(serde::de::Error::custom("invalid Routine request UUID"));
    }
    Ok(id)
}
