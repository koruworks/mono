// SPDX-License-Identifier: MPL-2.0

//! Neutral Bot decision records. Shape, identity and encoding are not authority.
//!
//! These types start no wait, release no capacity, interpret no Loro bytes and
//! expose no command. The ledger must check retained rows and current authority;
//! the document owner must check causal evidence. Historical receipt equality
//! must never be confused with permission for a fresh effect.

use serde::{Deserialize, Serialize};

mod binding;
mod bounded;
mod enrollment;
mod identity;
mod outcome;
mod publication;
mod resume;
mod state;

pub use binding::*;
pub use bounded::*;
pub use enrollment::*;
pub use identity::*;
pub use outcome::decision_outcome_digest;
pub use publication::*;
pub use resume::*;
pub use state::*;

pub const DECISION_FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const DECISION_PAYLOAD_BYTES: usize = 65_536;
pub const DECISION_EVIDENCE_BYTES: usize = 4096;
pub const DECISION_RECEIPT_BYTES: usize = 65_536;
pub const DECISION_DELTA_BYTES: usize = 262_144;
pub const DECISION_WAIT_MILLIS: i64 = 600_000;
pub const DECISION_WAIT_LIMIT: usize = 50;

/// Content-free errors: rejected input must never appear in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionError {
    Invalid,
    TooLarge,
}
impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid decision record",
            Self::TooLarge => "decision record too large",
        })
    }
}
impl std::error::Error for DecisionError {}
impl From<crate::RoutineDigestError> for DecisionError {
    fn from(e: crate::RoutineDigestError) -> Self {
        match e {
            crate::RoutineDigestError::Invalid => Self::Invalid,
            crate::RoutineDigestError::TooLarge => Self::TooLarge,
        }
    }
}

/// Local validation only. No implementation can establish fresh permission.
pub trait DecisionValidate {
    fn validate(&self) -> Result<(), DecisionError>;
}

fn require(value: bool) -> Result<(), DecisionError> {
    value.then_some(()).ok_or(DecisionError::Invalid)
}

/// Read an already bounded complete frame. Callers must bound transport/SQL
/// allocation before constructing this slice. Nested byte/list visitors also
/// refuse growth at their own bound; no unbounded intermediate JSON Value.
pub fn decode_decision_frame<T: serde::de::DeserializeOwned + DecisionValidate>(
    bytes: &[u8],
) -> Result<T, DecisionError> {
    if bytes.len() > DECISION_FRAME_BYTES {
        return Err(DecisionError::TooLarge);
    }
    let value: T = serde_json::from_slice(bytes).map_err(|_| DecisionError::Invalid)?;
    value.validate()?;
    Ok(value)
}

/// Serialize with a bounded writer, including the complete caller-supplied
/// envelope when used on an envelope type. It cannot bound a separate RPC ID.
pub fn encode_decision_frame<T: Serialize + DecisionValidate>(
    value: &T,
) -> Result<Vec<u8>, DecisionError> {
    value.validate()?;
    bounded::encode(value, DECISION_FRAME_BYTES)
}

/// Storage owners have narrower column bounds than the outer frame. This
/// only bounds serialization; the owning record validator remains mandatory.
#[doc(hidden)]
pub fn encode_decision_value<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, DecisionError> {
    if limit > DECISION_FRAME_BYTES {
        return Err(DecisionError::TooLarge);
    }
    bounded::encode(value, limit)
}

#[cfg(test)]
mod tests;
