// SPDX-License-Identifier: MPL-2.0

//! Internal, bounded persistence batches. None of these values grants authority.
//! The AI writer supplies already normalized effects; native event bodies are not stored.

use crate::{
    AgentNotice, AiEvent, AnswersRecord, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlock, PendingRetry, PendingTurn, ToolActivity, ToolInvocationId,
};
pub use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotDeliveryReceipt, BotExecutionFence, BotSubmission,
    RunAttemptId, RunId, SessionId,
};
use serde::Serialize;

pub const MAX_REPLAY_EVENTS: usize = 256;
pub const MAX_REPLAY_EFFECTS: usize = 1024;
pub const MAX_REPLAY_BYTES: usize = 4 * 1024 * 1024;

/// An AI-owned preparation, never a public caller's admission authority.
/// Retry and a terminal question answer preserve the shared assistant-only UI.
#[derive(Clone)]
pub enum BotChatPreparation {
    Turn(PendingTurn),
    Retry {
        retry: PendingRetry,
        prior_attempt: InferenceAttemptId,
    },
    AnsweredContinuation {
        retry: PendingRetry,
        question: BotQuestionContinuation,
    },
}

#[derive(Clone)]
pub struct BotQuestionContinuation {
    pub prior_attempt: InferenceAttemptId,
    pub invocation: ToolInvocationId,
    pub source: BotToolSource,
    /// Already validated by the live definition; secrets must be withheld.
    pub answers: AnswersRecord,
}

impl BotChatPreparation {
    pub fn attempt(&self) -> &InferenceAttempt {
        match self {
            Self::Turn(turn) => &turn.attempt,
            Self::Retry { retry, .. } | Self::AnsweredContinuation { retry, .. } => &retry.attempt,
        }
    }
    pub fn assistant(&self) -> &Message {
        match self {
            Self::Turn(turn) => &turn.assistant_message,
            Self::Retry { retry, .. } | Self::AnsweredContinuation { retry, .. } => {
                &retry.assistant_message
            }
        }
    }
}

#[derive(Debug)]
pub enum BotPreparationError {
    Admission(super::BotError),
    Persistence(crate::AiError),
}
impl From<super::BotError> for BotPreparationError {
    fn from(value: super::BotError) -> Self {
        Self::Admission(value)
    }
}
impl From<crate::AiError> for BotPreparationError {
    fn from(value: crate::AiError) -> Self {
        Self::Persistence(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BotToolSourceKind {
    Tool,
    Approval,
    Questions,
}
impl BotToolSourceKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Approval => "approval",
            Self::Questions => "questions",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BotToolSource {
    pub kind: BotToolSourceKind,
    pub id: String,
}

/// Closed normalized effects, sharing ordinary AI persistence semantics. An Event
/// cannot contain unnormalized AgentActivity/ToolActivity/AgentNotice/ToolCalls.
#[derive(Clone, Serialize)]
pub enum BotReplayEffect {
    Event {
        event: AiEvent,
        finalized_blocks: Vec<MessageBlock>,
        terminal_cleanup: Option<BotTerminalCleanup>,
    },
    CreateTool {
        source: BotToolSource,
        activity: ToolActivity,
    },
    AdvanceTool {
        source: BotToolSource,
        activity: ToolActivity,
    },
    Notice(AgentNotice),
    Mode(Option<String>),
}

/// Explicit shared-policy decision for a terminal source event. No default:
/// the mapper must name questions intentionally retained for the next message.
#[derive(Clone, Serialize)]
pub struct BotTerminalCleanup {
    pub retained_question_ids: Vec<ToolInvocationId>,
}

#[derive(Clone, Serialize)]
pub struct BotReplayEvent {
    pub sequence: u64,
    pub turn: Option<AgentTurnId>,
    /// Empty means explicitly classified as handled without durable AI effects.
    pub effects: Vec<BotReplayEffect>,
}

#[derive(Clone, Serialize)]
pub struct BotReplayBatch {
    pub ai_attempt: InferenceAttemptId,
    pub fence: BotExecutionFence,
    pub expected_after: Option<u64>,
    pub events: Vec<BotReplayEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotReplayDisposition {
    Applied,
    Duplicate,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotReplayStop {
    ReplayGap,
    ExecutionChanged,
    AiRestart,
    OwnerUnavailable,
}
impl BotReplayStop {
    pub const fn token(self) -> &'static str {
        match self {
            Self::ReplayGap => "replay_gap",
            Self::ExecutionChanged => "execution_changed",
            Self::AiRestart => "ai_restart",
            Self::OwnerUnavailable => "owner_unavailable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotReplayState {
    pub fence: Option<BotExecutionFence>,
    pub last_sequence: Option<u64>,
    pub stop_reason: Option<BotReplayStop>,
}

/// Opaque position within an owner-scoped delivery scan, never authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotDeliveryCursor {
    pub created_at_ms: i64,
    pub ai_attempt: InferenceAttemptId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotDeliveryPage {
    pub receipts: Vec<BotDeliveryReceipt>,
    pub next_cursor: Option<BotDeliveryCursor>,
}
