// SPDX-License-Identifier: MPL-2.0

use crate::{CapabilitySet, ExecutableAvailability, ProviderKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Safe host-owned discovery projection. Raw executable presence remains local
/// to the window; this row describes only the registered managed adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderReadiness {
    pub kind: ProviderKind,
    pub availability: ExecutableAvailability,
    pub capabilities: CapabilitySet,
    pub failure: Option<ProviderReadinessFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderReadinessFailure {
    pub stage: ProviderStage,
    pub diagnostic: ProviderDiagnostic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStage {
    Discovery,
    Version,
    Spawn,
    Initialize,
    Read,
    Write,
    Submit,
    Cancel,
    Approval,
    Shutdown,
}

/// The message a session failure carries when the refused thing was the
/// reasoning level rather than the model: the code alone is the refused
/// model's (`provider_model_unavailable`), so a chat could not say which.
pub const REASONING_REJECTED: &str = "reasoning_rejected";

/// Closed, content-free diagnostic detail suitable for developer support logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDiagnostic {
    MissingExecutable,
    AuthenticationRequired,
    RuntimeUnavailable,
    InvalidVersion,
    VersionRejected,
    HandshakeRejected,
    Timeout,
    Spawn,
    Io,
    MalformedFrame,
    FrameTooLarge,
    Backpressure,
    UnexpectedExit,
    InvalidState,
    InvalidInput,
    /// The agent did not offer, or did not apply, the reasoning level a
    /// session was started with (agent-models D19). The failure's code is
    /// `ProviderModelUnavailable`; this says it was the level, not the model.
    ReasoningRejected,
}
