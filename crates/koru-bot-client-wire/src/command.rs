// SPDX-License-Identifier: MPL-2.0

//! What a linked device may ask the owner to do, and to read.
//!
//! Each command is one of the owner's existing local commands: its members
//! are that command's own public members, and [`ClientCommand::bot_command`]
//! gives the local form of the ones the Bot authority runs. The owner applies
//! an allow-list. A kind this build does not know decodes as
//! [`ClientCommand::Unsupported`], so the owner can answer it with a receipt
//! (`executing_computer_only`) instead of resetting the stream; it is never
//! written.
//!
//! **Stays on the executing computer**, and so has no kind here: creating a
//! Bot, access and selected pages, connection, vault and grant commands,
//! Knowledge and Memory, the Role page, permanent delete, the working folder,
//! browser grants, attaching to or taking over a session.

use std::fmt;

use koru_ai_core::bots::{BotCommand, BotConfig, BotId, BotRequestId, RunId};
use koru_ai_core::{ChatId, InferenceAttemptId, MessageId, ToolInvocationId};
use koru_ai_wire::{RetryTurnRequest, SendTurnRequest, SteerAttemptRequest, ToolDecisionRequest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::negotiate::capability;

/// One intent of the person at a linked device.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientCommand {
    /// Send a message into a Bot conversation. Text and page references
    /// only: attachments are refused in version 1 (D19).
    SendTurn { request: SendTurnRequest },
    RetryTurn {
        chat_id: ChatId,
        request: RetryTurnRequest,
    },
    /// Stop a running reply.
    CancelAttempt { attempt_id: InferenceAttemptId },
    /// Send text into the reply that is running.
    Steer {
        attempt_id: InferenceAttemptId,
        request: SteerAttemptRequest,
    },
    CreateChat {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    PauseBot {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    ResumeBot {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
    },
    PauseTeam {
        request_id: BotRequestId,
        expected_team_revision: u64,
    },
    ResumeTeam {
        request_id: BotRequestId,
        expected_team_revision: u64,
    },
    /// Answer a "Needs you": approve the pending call with `decision`, or
    /// deny it (`decision.choice` then names the offered denial, if any).
    AnswerDecision {
        tool_id: ToolInvocationId,
        approve: bool,
        decision: ToolDecisionRequest,
    },
    /// Name, model, reasoning, mode and Fast. The owner refuses a change of
    /// the working folder or the Role page.
    UpdateBot {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        config: BotConfig,
    },
    /// A kind this build does not know. Read, never written.
    #[serde(other)]
    Unsupported,
}

impl ClientCommand {
    /// The command's `kind`.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::SendTurn { .. } => "send_turn",
            Self::RetryTurn { .. } => "retry_turn",
            Self::CancelAttempt { .. } => "cancel_attempt",
            Self::Steer { .. } => "steer",
            Self::CreateChat { .. } => "create_chat",
            Self::PauseBot { .. } => "pause_bot",
            Self::ResumeBot { .. } => "resume_bot",
            Self::PauseTeam { .. } => "pause_team",
            Self::ResumeTeam { .. } => "resume_team",
            Self::AnswerDecision { .. } => "answer_decision",
            Self::UpdateBot { .. } => "update_bot",
            Self::Unsupported => "unsupported",
        }
    }

    /// The capability that must be negotiated before this kind is sent.
    /// `None` for a kind this build does not know.
    pub const fn capability(&self) -> Option<&'static str> {
        match self {
            Self::SendTurn { .. }
            | Self::RetryTurn { .. }
            | Self::CancelAttempt { .. }
            | Self::Steer { .. }
            | Self::CreateChat { .. } => Some(capability::CMD_CHAT),
            Self::PauseBot { .. } | Self::ResumeBot { .. } => Some(capability::CMD_PAUSE),
            Self::PauseTeam { .. } | Self::ResumeTeam { .. } => Some(capability::CMD_TEAM_PAUSE),
            Self::AnswerDecision { .. } => Some(capability::CMD_DECISIONS),
            Self::UpdateBot { .. } => Some(capability::CMD_CONFIG),
            Self::Unsupported => None,
        }
    }

    /// The request id of the embedded local command, where it has one. The
    /// owner refuses a command whose wrapper `request_id` differs from it:
    /// there is one request id per intent.
    pub fn embedded_request_id(&self) -> Option<Uuid> {
        match self {
            Self::SendTurn { request } => Some(request.operation_id),
            Self::RetryTurn { request, .. } => Some(request.operation_id),
            Self::Steer { request, .. } => Some(request.operation_id),
            Self::CreateChat { request_id, .. }
            | Self::PauseBot { request_id, .. }
            | Self::ResumeBot { request_id, .. }
            | Self::PauseTeam { request_id, .. }
            | Self::ResumeTeam { request_id, .. }
            | Self::UpdateBot { request_id, .. } => Some(request_id.as_uuid()),
            Self::CancelAttempt { .. } | Self::AnswerDecision { .. } | Self::Unsupported => None,
        }
    }

    /// True for a `send_turn` that names attachments, which version 1
    /// refuses.
    pub fn carries_attachments(&self) -> bool {
        matches!(self, Self::SendTurn { request } if !request.attachment_ids.is_empty())
    }

    /// The local Bot command this is, for the kinds the Bot authority runs.
    /// `None` for the chat kinds, which the chat port runs, and for a kind
    /// this build does not know.
    pub fn bot_command(&self) -> Option<BotCommand> {
        match self {
            Self::CreateChat { request_id, bot_id } => Some(BotCommand::CreateChat {
                request_id: *request_id,
                bot_id: *bot_id,
            }),
            Self::PauseBot {
                request_id,
                bot_id,
                expected_revision,
            } => Some(BotCommand::Pause {
                request_id: *request_id,
                bot_id: *bot_id,
                expected_revision: *expected_revision,
            }),
            Self::ResumeBot {
                request_id,
                bot_id,
                expected_revision,
            } => Some(BotCommand::Resume {
                request_id: *request_id,
                bot_id: *bot_id,
                expected_revision: *expected_revision,
            }),
            Self::PauseTeam {
                request_id,
                expected_team_revision,
            } => Some(BotCommand::PauseTeam {
                request_id: *request_id,
                expected_team_revision: *expected_team_revision,
            }),
            Self::ResumeTeam {
                request_id,
                expected_team_revision,
            } => Some(BotCommand::ResumeTeam {
                request_id: *request_id,
                expected_team_revision: *expected_team_revision,
            }),
            Self::UpdateBot {
                request_id,
                bot_id,
                expected_revision,
                config,
            } => Some(BotCommand::Update {
                request_id: *request_id,
                bot_id: *bot_id,
                expected_revision: *expected_revision,
                config: config.clone(),
            }),
            Self::SendTurn { .. }
            | Self::RetryTurn { .. }
            | Self::CancelAttempt { .. }
            | Self::Steer { .. }
            | Self::AnswerDecision { .. }
            | Self::Unsupported => None,
        }
    }
}

impl fmt::Debug for ClientCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ClientCommand({})", self.kind())
    }
}

/// A read a linked device may ask of the owner. It changes nothing.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientQuery {
    /// One Run of a Bot conversation, with a window of its history.
    ReadRun {
        bot_id: BotId,
        chat_id: ChatId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<MessageId>,
        run_id: RunId,
        from: u64,
        len: u32,
    },
    /// What became of one earlier request.
    ReadOperation {
        request_id: BotRequestId,
        bot_id: BotId,
    },
    /// A kind this build does not know. Read, never written.
    #[serde(other)]
    Unsupported,
}

impl ClientQuery {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ReadRun { .. } => "read_run",
            Self::ReadOperation { .. } => "read_operation",
            Self::Unsupported => "unsupported",
        }
    }

    /// The capability that must be negotiated before this kind is sent.
    pub const fn capability(&self) -> Option<&'static str> {
        match self {
            Self::ReadRun { .. } | Self::ReadOperation { .. } => Some(capability::QUERY_RUN),
            Self::Unsupported => None,
        }
    }

    /// The local Bot command this read is.
    pub fn bot_command(&self) -> Option<BotCommand> {
        match self {
            Self::ReadRun {
                bot_id,
                chat_id,
                message_id,
                run_id,
                from,
                len,
            } => Some(BotCommand::ReadRun {
                bot_id: *bot_id,
                chat_id: *chat_id,
                message_id: *message_id,
                run_id: *run_id,
                from: *from,
                len: *len,
            }),
            Self::ReadOperation { request_id, bot_id } => Some(BotCommand::ReadOperation {
                request_id: *request_id,
                bot_id: *bot_id,
            }),
            Self::Unsupported => None,
        }
    }
}

impl fmt::Debug for ClientQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ClientQuery({})", self.kind())
    }
}
