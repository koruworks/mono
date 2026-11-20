// SPDX-License-Identifier: MPL-2.0
//! Retained scalar identities shared with standalone Connections. No decision
//! execution, enrollment, resume, publication or authority codec remains here.
use serde::{Deserialize, Serialize};
mod identity;
pub use identity::{DecisionDigest, DecisionId, DecisionRevision};

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

fn require(value: bool) -> Result<(), DecisionError> {
    value.then_some(()).ok_or(DecisionError::Invalid)
}

/// Stored migration discriminator retained for schema compatibility checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    KoruEdit,
    NativeApproval,
    PrivateOperation,
}
