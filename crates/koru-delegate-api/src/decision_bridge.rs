// SPDX-License-Identifier: MPL-2.0

//! Private decision enrollment bridge: route constants, the separate auth
//! domain and the strict envelope codec, shared by the delegate host (client)
//! and the gateway (server). No handler is installed here. A decoded reply is
//! evidence to compare with the request, never authority by itself, and no
//! codec failure is evidence that an enrollment is missing.
use koru_delegate_core::decisions::{
    BotWaitEnrollmentReply, DecisionValidate, EnrollBotWaitCleanup, ReadBotWaitCleanup,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::Write;
use uuid::Uuid;

mod auth;
pub use auth::DecisionBridgeAuthority;

/// The complete strict frame bound (contract section 6).
pub const DECISION_BRIDGE_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// One shared total deadline across every phase; this vocabulary starts no timer.
pub const DECISION_BRIDGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
pub const DECISION_BRIDGE_METHOD: &str = "POST";
/// The contract's literal private paths (O115), distinct from the Routine prefix.
pub const DECISION_ENROLL_ROUTE: &str = "/private/bots/decisions/enroll-wait-cleanup";
pub const DECISION_READ_ROUTE: &str = "/private/bots/decisions/read-wait-cleanup";

fn v4(id: Uuid) -> bool {
    id.get_version() == Some(uuid::Version::Random) && id.get_variant() == uuid::Variant::RFC4122
}

/// Content-free codec failures. None proves absence of an enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionBridgeCodecError {
    Invalid,
    TooLarge,
    Mismatch,
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for koru_delegate_core::decisions::EnrollBotWaitCleanup {}
    impl Sealed for koru_delegate_core::decisions::ReadBotWaitCleanup {}
    impl Sealed for koru_delegate_core::decisions::BotWaitEnrollmentReply {}
}

/// Only the bridge's closed DTOs implement this. Validation is shape only.
pub trait DecisionBridgePayload: sealed::Sealed {
    fn valid(&self) -> bool;
}
/// A reply's exact correlation with the request it answers.
pub trait DecisionBridgeReply<Request>: DecisionBridgePayload {
    fn matches_request(&self, request: &Request) -> bool;
}

impl DecisionBridgePayload for EnrollBotWaitCleanup {
    fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}
impl DecisionBridgePayload for ReadBotWaitCleanup {
    fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}
impl DecisionBridgePayload for BotWaitEnrollmentReply {
    fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}
/// Enroll answers Enrolled for the exact key or Refused; it never answers
/// Missing (only Read can, after an authenticated exact subject read).
impl DecisionBridgeReply<EnrollBotWaitCleanup> for BotWaitEnrollmentReply {
    fn matches_request(&self, request: &EnrollBotWaitCleanup) -> bool {
        BotWaitEnrollmentReply::matches_request(self, &request.key, false)
    }
}
impl DecisionBridgeReply<ReadBotWaitCleanup> for BotWaitEnrollmentReply {
    fn matches_request(&self, request: &ReadBotWaitCleanup) -> bool {
        BotWaitEnrollmentReply::matches_request(self, &request.key, true)
    }
}

/// `{"version":1,"request_id":"<canonical v4>","payload":...}`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBridgeEnvelope<T> {
    #[serde(deserialize_with = "version_one")]
    pub version: u8,
    #[serde(deserialize_with = "request_uuid")]
    pub request_id: Uuid,
    pub payload: T,
}
impl<T> std::fmt::Debug for DecisionBridgeEnvelope<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionBridgeEnvelope")
            .field("version", &self.version)
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl<T: DecisionBridgePayload> DecisionBridgeEnvelope<T> {
    pub fn new(payload: T) -> Result<Self, DecisionBridgeCodecError> {
        let envelope = Self {
            version: 1,
            request_id: Uuid::new_v4(),
            payload,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// The server's reply to `request`, under the same request ID.
    pub fn reply_to<R: DecisionBridgePayload>(
        request: &DecisionBridgeEnvelope<R>,
        payload: T,
    ) -> Result<Self, DecisionBridgeCodecError>
    where
        T: DecisionBridgeReply<R>,
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

    pub fn validate(&self) -> Result<(), DecisionBridgeCodecError> {
        if self.version != 1 || !v4(self.request_id) || !self.payload.valid() {
            return Err(DecisionBridgeCodecError::Invalid);
        }
        Ok(())
    }

    pub fn validate_reply_to<R: DecisionBridgePayload>(
        &self,
        request: &DecisionBridgeEnvelope<R>,
    ) -> Result<(), DecisionBridgeCodecError>
    where
        T: DecisionBridgeReply<R>,
    {
        self.validate()?;
        request.validate()?;
        if self.request_id != request.request_id || !self.payload.matches_request(&request.payload)
        {
            return Err(DecisionBridgeCodecError::Mismatch);
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
            return Err(std::io::Error::other("decision envelope too large"));
        };
        self.remaining = remaining;
        self.written += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<T: DecisionBridgePayload + Serialize> DecisionBridgeEnvelope<T> {
    /// Count the complete encoded envelope before allocating its body.
    pub fn encode(&self) -> Result<Vec<u8>, DecisionBridgeCodecError> {
        self.validate()?;
        let mut count = Count {
            remaining: DECISION_BRIDGE_BODY_LIMIT,
            written: 0,
            exceeded: false,
        };
        if serde_json::to_writer(&mut count, self).is_err() {
            return Err(if count.exceeded {
                DecisionBridgeCodecError::TooLarge
            } else {
                DecisionBridgeCodecError::Invalid
            });
        }
        let mut bytes = Vec::with_capacity(count.written);
        serde_json::to_writer(&mut bytes, self).map_err(|_| DecisionBridgeCodecError::Invalid)?;
        Ok(bytes)
    }
}

impl<T: DecisionBridgePayload + DeserializeOwned> DecisionBridgeEnvelope<T> {
    /// Rejects an oversize body before deserializing any field. The HTTP
    /// owners enforce the same limit while streaming.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecisionBridgeCodecError> {
        if bytes.len() > DECISION_BRIDGE_BODY_LIMIT {
            return Err(DecisionBridgeCodecError::TooLarge);
        }
        let envelope: Self =
            serde_json::from_slice(bytes).map_err(|_| DecisionBridgeCodecError::Invalid)?;
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
            "unsupported decision bridge version",
        ))
    }
}
fn request_uuid<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(d)?;
    let id = Uuid::parse_str(&text)
        .map_err(|_| serde::de::Error::custom("invalid decision request UUID"))?;
    if !v4(id) || id.to_string() != text {
        return Err(serde::de::Error::custom("invalid decision request UUID"));
    }
    Ok(id)
}

#[cfg(test)]
pub(crate) mod tests;
