// SPDX-License-Identifier: MPL-2.0

//! Strict new command without changing ordinary PendingInput compatibility.
use koru_session_protocol::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerPendingInput {
    pub session_id: SessionId,
    pub controller_lease: ControllerLeaseToken,
    pub expected_turn: AgentTurnId,
    #[serde(with = "StrictPendingInput")]
    pub input: PendingInput,
}

/// Initial-only managed input. An active turn is never implicitly steered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginPendingInput {
    pub session_id: SessionId,
    pub controller_lease: ControllerLeaseToken,
    #[serde(with = "StrictPendingInput")]
    pub input: PendingInput,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "PendingInput", deny_unknown_fields)]
struct StrictPendingInput {
    idempotency_key: IdempotencyKey,
    text: String,
    attachment_ids: Vec<AttachmentId>,
    #[serde(default)]
    references: Vec<PathReference>,
    #[serde(default)]
    mode: Option<AgentModeId>,
    #[serde(default)]
    fast: Option<bool>,
    #[serde(default)]
    reasoning: Option<LaunchReasoning>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    model: Option<ModelName>,
}

#[cfg(test)]
mod tests;
