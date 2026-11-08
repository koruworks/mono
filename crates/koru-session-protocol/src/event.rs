// SPDX-License-Identifier: MPL-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AgentTurnId, ApprovalId, AttachmentId, AttentionState, CachedInputRelationship, CancelReason,
    ErrorCode, HandoverState, ModelName, ProviderKind, SessionFailure, SessionId, StreamSequence,
    ToolCallId, ToolFailure, ToolKind,
};

/// Lifecycle and low-volume facts for one session. Render content has its own
/// bounded [`crate::RenderUpdate`] stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Protocol v23. The live session's current command listing, including an
    /// authoritative empty listing.
    CommandsUpdated {
        commands: Vec<crate::AgentCommand>,
    },
    Output {
        byte_count: u64,
    },
    Bell,
    Started,
    Exited {
        code: Option<i32>,
    },
    Failed {
        error: SessionFailure,
    },
    WorkingDirectoryChanged {
        working_directory: String,
    },
    TitleChanged {
        title: String,
    },
    AttentionChanged {
        attention: AttentionState,
    },
    AttachmentCreated {
        attachment_id: AttachmentId,
    },
    AttachmentRemoved {
        attachment_id: AttachmentId,
    },
    AttachmentRejected {
        attachment_id: AttachmentId,
        code: ErrorCode,
    },
    AttachmentDelivered {
        attachment_id: AttachmentId,
        turn_id: AgentTurnId,
    },
    /// Protocol v14. The controller handover moved: someone asked, the holder
    /// released, the lease was taken, or it was given back. A new tag, so a
    /// host never sends it to a peer that negotiated less than v14.
    HandoverChanged {
        handover: HandoverState,
    },
}

impl SessionEvent {
    /// First negotiated protocol version that can read this event. A host
    /// forwards nothing a subscriber could not parse: an unknown tag would
    /// end the subscriber's connection, not one event.
    pub fn minimum_protocol_version(&self) -> u32 {
        match self {
            Self::CommandsUpdated { .. } => crate::PATH_REFERENCES_PROTOCOL_VERSION,
            Self::HandoverChanged { .. } => crate::DELEGATION_PROTOCOL_VERSION,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SequencedSessionEvent {
    pub session_id: SessionId,
    pub sequence: StreamSequence,
    pub event: SessionEvent,
}

/// One bounded chunk for an explicitly authorized non-visual output
/// subscription. Render subscribers consume `RenderUpdate` instead.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RawOutputChunk {
    pub session_id: SessionId,
    pub sequence: StreamSequence,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for RawOutputChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawOutputChunk")
            .field("session_id", &self.session_id)
            .field("sequence", &self.sequence)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

/// One usage report for one turn.
///
/// **The report is the total for `turn_id` so far, and a later report for
/// the same turn replaces the earlier one.** Whether the provider sends
/// deltas, running totals or replacements is the adapter's business to
/// declare in its compatibility note and normalize here; above this type
/// there is one total per turn. A missing count is `None` — unavailable is
/// never zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    /// Protocol v14. How `cached_input_tokens` relates to `input_tokens`,
    /// as the adapter declared it. Absent from an older peer, which reads as
    /// `Undeclared` — incomplete, never guessed.
    #[serde(default)]
    pub cached_input: CachedInputRelationship,
    /// Protocol v14. The model the provider reported for this turn, or
    /// `None` when it said nothing. Never a copy of the model that was asked
    /// for: that lives on the session summary as `requested`.
    #[serde(default)]
    pub model: Option<ModelName>,
}

/// Provider-neutral managed-session facts. Provider IDs and native wire values
/// never cross this boundary.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Protocol v23. The provider's live listing, independent of any turn.
    CommandsUpdated {
        commands: Vec<crate::AgentCommand>,
    },
    TurnStarted {
        turn_id: AgentTurnId,
    },
    MessageDelta {
        turn_id: AgentTurnId,
        text: String,
    },
    ToolStarted {
        turn_id: AgentTurnId,
        tool_call_id: ToolCallId,
        label: String,
        /// Protocol v17 (UC17). What the agent asked the tool to do, as the
        /// adapter read it from the call — a command line, the paths of a
        /// change, an MCP tool's arguments — bounded to
        /// [`TOOL_CONTENT_CHARS`]. `None` when the provider gave none, and
        /// from an older peer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    ToolCompleted {
        turn_id: AgentTurnId,
        tool_call_id: ToolCallId,
        succeeded: bool,
        /// Protocol v14. Which tool it was, as the adapter classifies it —
        /// never the provider's own name for it. Absent from an older peer,
        /// which reads as `None`: unknown, never guessed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<ToolKind>,
        /// Protocol v14. Why it did not work, as the adapter classified it
        /// from the structured fields it already reads — never the
        /// provider's error text, which does not cross this boundary at
        /// all. `None` when it succeeded, and `None` from an older peer,
        /// which the run reads as [`ToolFailure::Unclassified`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<ToolFailure>,
        /// Protocol v17. The tool's own account of why it failed, as the
        /// provider reported it, bounded to [`TOOL_DETAIL_CHARS`]
        /// characters — the first thing a person asks of a failed row, and
        /// the one piece of provider text that crosses here. `None` when it
        /// succeeded, when the provider said nothing readable, and from an
        /// older peer. Never the arguments, never the results.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Protocol v17 (UC17). What the tool produced, as the adapter read
        /// it — a command's output, a change's diff, an MCP result — bounded
        /// to [`TOOL_CONTENT_CHARS`]. `None` when the provider gave none,
        /// and from an older peer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    ApprovalRequired {
        turn_id: AgentTurnId,
        approval_id: ApprovalId,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choices: Vec<crate::ApprovalChoice>,
    },
    ApprovalResolved {
        turn_id: AgentTurnId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<crate::ApprovalChoiceId>,
    },
    ApprovalExpired {
        turn_id: AgentTurnId,
        approval_id: ApprovalId,
    },
    WaitingForInput {
        turn_id: AgentTurnId,
    },
    AttachmentAccepted {
        turn_id: AgentTurnId,
        attachment_id: AttachmentId,
    },
    AttachmentRejected {
        turn_id: AgentTurnId,
        attachment_id: AttachmentId,
        code: ErrorCode,
    },
    Usage {
        turn_id: AgentTurnId,
        usage: AgentUsage,
    },
    ChildSessionCreated {
        turn_id: AgentTurnId,
        child_session_id: SessionId,
    },
    TurnCompleted {
        turn_id: AgentTurnId,
    },
    TurnCancelled {
        turn_id: AgentTurnId,
        /// Protocol v14. Why the turn was cancelled. A peer that predates the
        /// field reads `Run`, which is what every cancellation meant before
        /// a takeover could end a step without ending the job.
        #[serde(default)]
        reason: CancelReason,
    },
    TurnFailed {
        turn_id: AgentTurnId,
        error: SessionFailure,
    },
    ProviderState {
        provider: ProviderKind,
        state: crate::ManagedState,
    },
    /// Protocol v19 (koru-ai D7). The agent moved its approval mode itself
    /// during the turn — Claude leaving plan mode once a plan is approved,
    /// or taking another mode when the one asked for is not offered by its
    /// model. A new tag, so the host sends it only to a v19 peer.
    ModeChanged {
        turn_id: AgentTurnId,
        mode: crate::AgentModeId,
    },
    /// Protocol v20 (koru-ai D69). A short status line from the agent that
    /// is not part of its reply — a hook that failed on the person's machine,
    /// a warning about the session — bounded to [`AGENT_NOTICE_CHARS`]
    /// characters. One the agent said before any turn is said with the
    /// next. A new tag, so the host sends it only to a v20 peer.
    Notice {
        turn_id: AgentTurnId,
        text: String,
    },
    /// Protocol v21 (koru-ai D108). The agent stopped to ask the person —
    /// Codex's `request_user_input`, Claude's `AskUserQuestion` — bounded by
    /// [`crate::bound_questions`]. A set is answered once, through
    /// `answer_questions`, and closed by [`AgentEvent::QuestionsResolved`].
    /// `blocking: false` is a set the agent does not wait on: its turn runs
    /// on, and the set may be answered until the agent closes it. A new tag,
    /// so the host sends it only to a v21 peer; a session created by an
    /// older peer is never asked at all (the adapter declines for it).
    ///
    /// `delivery: next_message` is a set the agent asked in its reply and
    /// moved on from (Codex's `request_user_input_async`): never blocking,
    /// it outlives its turn, and its answers go as the person's next
    /// message — [`crate::QuestionsReply`] on `send_pending_input`, or
    /// `answer_questions` while its turn still runs, which steers them in.
    /// A skip sends the agent nothing; another message passes it over and
    /// it expires.
    QuestionsRequired {
        turn_id: AgentTurnId,
        request_id: crate::QuestionRequestId,
        questions: Vec<crate::AgentQuestion>,
        #[serde(default = "blocking_default")]
        blocking: bool,
        #[serde(default, skip_serializing_if = "crate::QuestionsDelivery::is_in_turn")]
        delivery: crate::QuestionsDelivery,
    },
    /// Protocol v21. How a set of questions ended: answered — a secret's
    /// answer withheld — skipped, or expired with its turn.
    QuestionsResolved {
        turn_id: AgentTurnId,
        request_id: crate::QuestionRequestId,
        outcome: crate::QuestionsOutcome,
    },
}

fn blocking_default() -> bool {
    true
}

/// The most characters an agent's notice keeps; the rest is cut at a
/// character boundary with an ellipsis.
pub const AGENT_NOTICE_CHARS: usize = 500;

/// `text` bounded for [`AgentEvent::Notice`]: trimmed, at most
/// [`AGENT_NOTICE_CHARS`] characters, `None` when nothing is left.
pub fn bound_agent_notice(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= AGENT_NOTICE_CHARS {
        return Some(text.to_owned());
    }
    let mut bounded: String = text.chars().take(AGENT_NOTICE_CHARS - 1).collect();
    bounded.push('…');
    Some(bounded)
}

impl AgentEvent {
    /// First negotiated protocol version that can read this event. A host
    /// forwards nothing a subscriber could not parse: an unknown tag would
    /// end the subscriber's connection, not one event.
    pub fn minimum_protocol_version(&self) -> u32 {
        match self {
            Self::CommandsUpdated { .. } => crate::PATH_REFERENCES_PROTOCOL_VERSION,
            Self::ModeChanged { .. } => crate::AGENT_MODES_PROTOCOL_VERSION,
            Self::Notice { .. } => crate::AGENT_NOTICES_PROTOCOL_VERSION,
            Self::QuestionsRequired { .. } | Self::QuestionsResolved { .. } => {
                crate::QUESTIONS_PROTOCOL_VERSION
            }
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SequencedAgentEvent {
    pub session_id: SessionId,
    pub sequence: StreamSequence,
    pub event: AgentEvent,
}

/// Atomic bounded recovery point for a managed transcript. `events` is the
/// retained normalized tail ending at `cursor`; `earlier_truncated` makes
/// eviction explicit instead of pretending the tail is complete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentEventObservation {
    pub events: Vec<SequencedAgentEvent>,
    pub cursor: StreamSequence,
    pub earlier_truncated: bool,
    /// The current session listing survives eviction from the bounded event tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<crate::AgentCommand>>,
    /// The matching lifecycle-event cursor for an atomic command-list watch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cursor: Option<StreamSequence>,
}

impl std::fmt::Debug for AgentEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, turn) = match self {
            Self::CommandsUpdated { .. } => ("CommandsUpdated", None),
            Self::TurnStarted { turn_id } => ("TurnStarted", Some(turn_id)),
            Self::MessageDelta { turn_id, .. } => ("MessageDelta", Some(turn_id)),
            Self::ToolStarted { turn_id, .. } => ("ToolStarted", Some(turn_id)),
            Self::ToolCompleted { turn_id, .. } => ("ToolCompleted", Some(turn_id)),
            Self::ApprovalRequired { turn_id, .. } => ("ApprovalRequired", Some(turn_id)),
            Self::ApprovalResolved { turn_id, .. } => ("ApprovalResolved", Some(turn_id)),
            Self::ApprovalExpired { turn_id, .. } => ("ApprovalExpired", Some(turn_id)),
            Self::WaitingForInput { turn_id } => ("WaitingForInput", Some(turn_id)),
            Self::AttachmentAccepted { turn_id, .. } => ("AttachmentAccepted", Some(turn_id)),
            Self::AttachmentRejected { turn_id, .. } => ("AttachmentRejected", Some(turn_id)),
            Self::Usage { turn_id, .. } => ("Usage", Some(turn_id)),
            Self::ChildSessionCreated { turn_id, .. } => ("ChildSessionCreated", Some(turn_id)),
            Self::TurnCompleted { turn_id } => ("TurnCompleted", Some(turn_id)),
            Self::TurnCancelled { turn_id, .. } => ("TurnCancelled", Some(turn_id)),
            Self::TurnFailed { turn_id, .. } => ("TurnFailed", Some(turn_id)),
            Self::ProviderState { .. } => ("ProviderState", None),
            Self::ModeChanged { turn_id, .. } => ("ModeChanged", Some(turn_id)),
            Self::Notice { turn_id, .. } => ("Notice", Some(turn_id)),
            Self::QuestionsRequired { turn_id, .. } => ("QuestionsRequired", Some(turn_id)),
            Self::QuestionsResolved { turn_id, .. } => ("QuestionsResolved", Some(turn_id)),
        };
        f.debug_struct(kind)
            .field("turn_id", &turn)
            .finish_non_exhaustive()
    }
}
