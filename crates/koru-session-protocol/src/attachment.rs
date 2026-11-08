// SPDX-License-Identifier: MPL-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTurnId, AttachmentId, ErrorCode, IdempotencyKey, SessionFailure, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSource {
    Clipboard,
    Drop,
    FilePicker,
    PublicUpload,
    AgentGenerated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentStatus {
    Ingesting,
    Pending,
    Rejected { error: AttachmentValidationError },
    Removed,
}

impl AttachmentStatus {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Ingesting,
                Self::Pending | Self::Rejected { .. } | Self::Removed
            ) | (Self::Pending, Self::Removed)
                | (Self::Rejected { .. }, Self::Removed)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    NativeStructured,
    QuotedPath,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivering {
        turn_id: AgentTurnId,
    },
    Delivered {
        turn_id: AgentTurnId,
    },
    Failed {
        turn_id: AgentTurnId,
        error: SessionFailure,
    },
}

impl DeliveryState {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        if self == next {
            return true;
        }
        match (self, next) {
            (Self::Pending, Self::Delivering { .. }) => true,
            (Self::Delivering { turn_id }, Self::Delivered { turn_id: next_turn })
            | (
                Self::Delivering { turn_id },
                Self::Failed {
                    turn_id: next_turn, ..
                },
            ) => turn_id == next_turn,
            (Self::Failed { turn_id, .. }, Self::Delivering { turn_id: next_turn }) => {
                turn_id == next_turn
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentValidationError {
    pub code: ErrorCode,
    /// Content-free developer detail. Never a path, filename, or input value.
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    pub id: AttachmentId,
    pub session_id: SessionId,
    pub kind: AttachmentKind,
    pub source: AttachmentSource,
    pub media_type: String,
    pub byte_count: u64,
    pub dimensions: Option<ImageDimensions>,
    pub status: AttachmentStatus,
    pub delivery_mode: DeliveryMode,
    pub delivery: DeliveryState,
    pub turn_id: Option<AgentTurnId>,
    pub preview_available: bool,
    pub created_at_ms: i64,
}

impl Attachment {
    /// Turn association is immutable. Replaying the same association is safe.
    pub fn associate_turn(&mut self, turn_id: AgentTurnId) -> Result<(), ErrorCode> {
        match self.turn_id {
            None => {
                self.turn_id = Some(turn_id);
                Ok(())
            }
            Some(existing) if existing == turn_id => Ok(()),
            Some(_) => Err(ErrorCode::InvalidTransition),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingInput {
    pub idempotency_key: IdempotencyKey,
    pub text: String,
    /// Ordered by the person composing the input.
    pub attachment_ids: Vec<AttachmentId>,
    /// v19 (koru-ai D7): the approval mode this turn runs in. The adapter
    /// moves the session to it before the prompt when it is elsewhere, and
    /// refuses the turn visibly when it cannot. `None` leaves the mode as
    /// it is. Sent only to a v19 host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::AgentModeId>,
    /// v19 (koru-ai D11): this turn runs with the model's Fast mode on or
    /// off. `None` leaves it as it is; a model without one ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<bool>,
    /// v20 (koru-ai D70): the reasoning level this turn runs at. The adapter
    /// moves the session to it before the prompt when it is elsewhere, and
    /// refuses the turn visibly when the agent cannot take it. `None` leaves
    /// the level as it is. Sent only to a v20 host: a v19 host would read
    /// and ignore it, and run the turn at the old level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<crate::LaunchReasoning>,
    /// v21 (koru-ai D121): the skills the person picked, by the name the
    /// agent listed them under, in the order they appear; the text says
    /// `$name` where each was. The adapter names each to the agent beside
    /// the text, and sends the text alone for one it no longer lists. Only
    /// to a host that negotiated `agent-commands`: an older one would read
    /// past it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// v22: the model this turn runs on, another of the session's agent's
    /// models chosen after the start. The adapter moves the session to it
    /// before the prompt when it is on another, and refuses the turn
    /// visibly when the agent cannot take it; the level and Fast mode the
    /// input carries are then the new model's. `None` leaves the model as
    /// it is. Sent only to a v22 host: an older one would read past it and
    /// run the turn on the session's old model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::ModelName>,
}

impl PendingInput {
    /// Whether only an `agent-commands` host can honor it.
    pub fn needs_agent_commands(&self) -> bool {
        !self.skills.is_empty()
    }

    /// Whether only a v19 host can honor it.
    pub fn needs_agent_modes(&self) -> bool {
        self.mode.is_some() || self.fast.is_some()
    }

    /// Whether only a v20 host can honor it.
    pub fn needs_turn_reasoning(&self) -> bool {
        self.reasoning.is_some()
    }

    /// Whether only a v22 host can honor it.
    pub fn needs_turn_model(&self) -> bool {
        self.model.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentDeliveryResult {
    pub attachment_id: AttachmentId,
    pub mode: DeliveryMode,
    pub state: DeliveryState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendPendingInputResult {
    pub turn_id: AgentTurnId,
    pub attachments: Vec<AttachmentDeliveryResult>,
}
