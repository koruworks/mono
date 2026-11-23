// SPDX-License-Identifier: MPL-2.0
//! The shared chat surface's engine carrier. Accepted input is not an AI
//! inference attempt: a live turn and its reserved reply arrive separately.
use koru_ai_core::{
    ChatId, ModelSelection,
    bots::{BotConversation, BotId},
};
pub use koru_session_protocol::{
    AgentEvent, ApprovalDecision, ApprovalIntent, QuestionAnswer, QuestionAnswerValue,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "BotThreadCommand")]
pub enum Command {
    Cards {
        thread: ChatId,
        after: Option<Uuid>,
    },
    Join {
        id: Uuid,
        thread: ChatId,
    },
    Rename {
        id: Uuid,
        thread: ChatId,
        revision: u64,
        title: String,
    },
    Branch {
        id: Uuid,
        thread: ChatId,
        branch: ChatId,
        through: Uuid,
        title: String,
    },
    Attention {
        thread: ChatId,
        after: Option<OperationRef>,
    },
    Continue {
        id: Uuid,
        thread: ChatId,
        operations: Vec<OperationRef>,
    },
    Read {
        thread: ChatId,
    },
    History {
        thread: ChatId,
        before: Option<u64>,
    },
    Since {
        thread: ChatId,
        after: u64,
    },
    Send {
        id: Uuid,
        bot: BotId,
        thread: ChatId,
        body: String,
        in_reply_to: Option<Uuid>,
        attachments: Vec<Value>,
        references: Vec<Value>,
    },
    Stop {
        id: Uuid,
        turn: Turn,
    },
    Observe {
        thread: ChatId,
        boot: Option<Uuid>,
        after: u64,
    },
    Settings {
        id: Uuid,
        bot: BotId,
        thread: ChatId,
        revision: u64,
        model: ModelSelection,
        mode: Option<String>,
        fast: Option<bool>,
    },
    Questions {
        thread: ChatId,
    },
    Answer {
        id: Uuid,
        question: Uuid,
        answer: Answer,
    },
    Reask {
        id: Uuid,
        thread: ChatId,
        question: Uuid,
    },
    Retry {
        id: Uuid,
        thread: ChatId,
        turn: Uuid,
    },
    /// Save one published Bot reply as a document in the Bot's home
    /// workspace. `id` is the save request and `document` the page it makes:
    /// both are chosen once and resent unchanged after an uncertain answer,
    /// so a retry finds the page instead of making another. `sequence` is
    /// where the reply sits in its thread; the engine's copy is what is
    /// saved, never text the window sends.
    SaveReply {
        id: Uuid,
        thread: ChatId,
        message: Uuid,
        sequence: u64,
        title: String,
        document: Uuid,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "BotThreadTurn")]
pub struct Turn {
    pub id: Uuid,
    pub generation: u64,
    pub worker: Uuid,
    pub session: Uuid,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadActiveTurn")]
pub struct ActiveTurn {
    pub id: Uuid,
    pub bot: Uuid,
    pub state: String,
    pub generation: u64,
    pub session: Option<Uuid>,
    pub worker: Option<Uuid>,
    pub reply_message: Option<Uuid>,
    pub created_at_ms: i64,
    pub details: Value,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadSnapshot")]
pub struct Snapshot {
    pub conversation: BotConversation,
    /// Thread metadata revision, independent of model/settings changes.
    pub revision: u64,
    pub settings_revision: u64,
    pub paused: bool,
    pub team_paused: bool,
    pub waiting_messages: u64,
    pub head_sequence: u64,
    pub turns: Vec<ActiveTurn>,
    #[serde(default)]
    pub joined: bool,
    #[serde(default)]
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub origin: Option<Origin>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadOrigin")]
pub struct Origin {
    pub thread: ChatId,
    pub bot: Uuid,
    pub name: String,
    pub title: String,
    pub main: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadParticipant")]
pub struct Participant {
    pub bot: Uuid,
    pub name: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadCard")]
pub struct Card {
    pub id: Uuid,
    pub sequence: u64,
    pub message: Uuid,
    pub kind: String,
    pub title: String,
    pub thread: Option<ChatId>,
    pub bot: Option<BotId>,
    pub bot_name: String,
    pub workspace: Option<koru_ai_core::WorkspaceId>,
    pub document: Option<koru_ai_core::DocumentId>,
    pub messages: u64,
    pub outputs: u64,
    pub state: String,
    /// Collaboration facts projected from dispatches and obligations, not
    /// inferred from the number of words messages in a thread.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatches: Vec<Dispatch>,
    /// A single completed firing. Several cards may share one Routine thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<RoutineResult>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadRoutineResult")]
pub struct RoutineResult {
    pub routine: Uuid,
    pub occurrence: Uuid,
    pub at_ms: i64,
    pub manual: bool,
    pub reply: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<koru_ai_core::bots::BotReplyPreview>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadDispatch")]
pub struct Dispatch {
    pub group: Uuid,
    /// Reserved response of the exact originating turn, including while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_reply: Option<Uuid>,
    pub message: Uuid,
    pub recipient: BotId,
    pub name: String,
    pub state: String,
    pub reply: Option<Uuid>,
    /// Exact answered-by message, bounded for the origin's read-only preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<koru_ai_core::bots::BotReplyPreview>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadContent")]
pub struct Content {
    pub body: String,
    pub attachments: Vec<Value>,
    pub references: Vec<Value>,
    pub in_reply_to: Option<Uuid>,
    pub deadline_ms: Option<i64>,
    pub reply_to: String,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadMessage")]
pub struct Message {
    pub id: Uuid,
    pub sequence: u64,
    pub sender: String,
    pub kind: String,
    pub content: Content,
    pub created_at_ms: i64,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadObservation")]
pub struct Observation {
    pub thread: Uuid,
    pub bot: Uuid,
    pub turn: Turn,
    pub event: AgentEvent,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadEvent")]
pub struct Event {
    pub cursor: u64,
    pub observation: Observation,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "BotThreadPrompt")]
pub enum Prompt {
    Approval {
        summary: String,
        choices: Vec<koru_session_protocol::ApprovalChoice>,
    },
    Questions {
        questions: Vec<koru_session_protocol::AgentQuestion>,
        blocking: bool,
        delivery: koru_session_protocol::QuestionsDelivery,
    },
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(rename = "BotThreadAnswer")]
pub enum Answer {
    Approval {
        decision: koru_session_protocol::ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    },
    Questions {
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    },
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadQuestion")]
pub struct Question {
    pub id: Uuid,
    pub turn: Uuid,
    pub prompt: Prompt,
    pub answer: Option<Answer>,
    pub state: String,
    pub delivery_state: String,
    pub provider_available: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(rename = "BotThreadResponse")]
pub enum Response {
    Cards {
        items: Vec<Card>,
        next: Option<Uuid>,
    },
    Branched {
        request: Uuid,
        thread: ChatId,
        replayed: bool,
    },
    Attention {
        items: Vec<OperationAttention>,
        next: Option<OperationRef>,
    },
    Continued {
        request: Uuid,
        messages: Vec<Uuid>,
        replayed: bool,
    },
    Read {
        snapshot: Snapshot,
    },
    History {
        messages: Vec<Message>,
        before: Option<u64>,
        after: u64,
    },
    Accepted {
        request: Uuid,
        message: Uuid,
        thread: Uuid,
        sequence: u64,
        replayed: bool,
    },
    Stopped {
        request: Uuid,
        cancelled: u64,
        replayed: bool,
    },
    Observed {
        boot: Uuid,
        cursor: u64,
        gap: bool,
        entries: Vec<Event>,
    },
    Settings {
        request: Uuid,
        revision: u64,
        model: ModelSelection,
        mode: Option<String>,
        fast: Option<bool>,
        replayed: bool,
    },
    Questions {
        questions: Vec<Question>,
    },
    Answered {
        request: Uuid,
        question: Uuid,
        continued: bool,
        replayed: bool,
    },
    Changed {
        request: Uuid,
        replayed: bool,
    },
    Saved {
        request: Uuid,
        document: Uuid,
        workspace: Uuid,
        title: String,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "BotThreadOperationRef")]
pub struct OperationRef {
    pub bot: Uuid,
    pub operation: Uuid,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "BotThreadOperationAttention")]
pub struct OperationAttention {
    pub key: OperationRef,
    pub workspace: koru_ai_core::WorkspaceId,
    pub document: koru_ai_core::DocumentId,
    pub kind: String,
    pub unresolved: bool,
    pub drained: bool,
}

// Prompts, answers and event text are never debug diagnostics.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotThreadCommand(<redacted>)")
    }
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotThreadResponse(<redacted>)")
    }
}

#[cfg(test)]
mod tests;
