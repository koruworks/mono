// SPDX-License-Identifier: MPL-2.0

//! One Bot asking another (M6): what the AI owner keeps and shows.
//!
//! Three things live here. The prepared ask, which the repository writes in
//! one transaction before the delegate hears of it. The refusals the AI owner
//! can give by itself, with their stable codes. And the two host-fed reads a
//! conversation view draws from: the request cards of a parent conversation
//! and the origin line of a child conversation. None of them holds a task, a
//! title or a page: a row here is identities, tokens and counts.
use super::*;
use crate::{MessageId, PendingTurn};
use koru_delegate_core::ChildRequestId;
use koru_delegate_core::children::{ChildClock, ChildRequestState};
use koru_delegate_core::decisions::BotWaitId;
use uuid::Uuid;

/// The most requests one ask carries, and the most one Run makes in total.
pub const BOT_CHILD_ASK_LIMIT: usize = koru_delegate_core::children::CHILD_REQUEST_LIMIT;

/// Why the AI owner refuses an ask before the delegate hears of it. The
/// delegate's own refusals (`bot_child_*`) are mapped to tool codes by the
/// worker; these are the checks ai.db can make against current rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BotChildAskRefusal {
    /// The asking Bot's "May ask other bots" is off.
    AskNotAllowed,
    /// The recipient does not exist, belongs to another person, another home
    /// workspace or another computer, or is archived or in Trash. One code:
    /// nothing tells these apart.
    RecipientUnavailable { index: u8 },
    /// The recipient is paused, or a pause or resume of it is unsettled.
    RecipientPaused { index: u8 },
    /// The recipient is the asking Bot.
    SelfAsk { index: u8 },
    /// The key was used by this Run with a different list.
    AskKeyConflict,
}
impl BotChildAskRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AskNotAllowed => "ask_not_allowed",
            Self::RecipientUnavailable { .. } => "recipient_unavailable",
            Self::RecipientPaused { .. } => "recipient_paused",
            Self::SelfAsk { .. } => "self_ask",
            Self::AskKeyConflict => "ask_key_conflict",
        }
    }
    /// The position of the refused item in the call, where one item is at fault.
    pub const fn index(self) -> Option<u8> {
        match self {
            Self::RecipientUnavailable { index }
            | Self::RecipientPaused { index }
            | Self::SelfAsk { index } => Some(index),
            Self::AskNotAllowed | Self::AskKeyConflict => None,
        }
    }
}

/// One item of an ask as the worker prepared it: the new conversation, the
/// turn that opens it (the task as the person's message would be, and the
/// pending reply), and the recipient's ordinary `start` submission with
/// `provenance.parent` set. `child_request` is also the submission's request
/// id, so the request and the recipient's receipt are one identity.
#[derive(Clone, PartialEq, Eq)]
pub struct BotChildAskItem {
    pub child_request: ChildRequestId,
    /// The conversation to create. Its title is the caller's text and is
    /// written to the conversation only, never to a child table.
    pub chat: Chat,
    pub turn: PendingTurn,
    pub intent: BotSubmissionIntent,
    /// The recipient's ordinary submission fingerprint.
    pub fingerprint: String,
    /// The recipient's configuration revision the worker read.
    pub expected_revision: u64,
    /// The recipient's own Library capture (O160: an asked Bot runs as
    /// itself): its Role, Knowledge and named pages as the submission's pins,
    /// and its own Memory, exactly as a task in its own conversation captures
    /// them. Context only: it grants no read or write beyond the reduced
    /// policy. Required to write a new ask; `None` only when the caller is
    /// answering a key it already used, which the repository replays before
    /// it would write anything.
    pub capture: Option<BotLibraryCapture>,
}
impl std::fmt::Debug for BotChildAskItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotChildAskItem")
            .field("child_request", &self.child_request)
            .field("chat", &self.chat.id)
            .finish_non_exhaustive()
    }
}

/// A whole ask, prepared for one ai.db transaction. Ids are minted by the
/// worker before the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotChildAsk {
    pub owner: BotOwner,
    pub parent_bot: BotId,
    pub parent_chat: ChatId,
    pub parent_run: RunId,
    pub parent_assistant_message: MessageId,
    pub ask_key: Uuid,
    /// 64 hexadecimal digits over the caller's request, as the tool sent it.
    pub ask_fingerprint: String,
    pub wait: BotWaitId,
    pub items: Vec<BotChildAskItem>,
}
impl BotChildAsk {
    /// Shape only: every limit that needs a current row is the writer's.
    pub fn validate(&self) -> Result<(), BotError> {
        let v4 = |id: Uuid| id.get_version() == Some(uuid::Version::Random);
        let distinct = |ids: Vec<Uuid>| {
            ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len()
        };
        if self.items.is_empty()
            || self.items.len() > BOT_CHILD_ASK_LIMIT
            || !v4(self.ask_key)
            || !v4(self.wait.as_uuid())
            || !v4(self.parent_run.as_uuid())
            || self.ask_fingerprint.len() != 64
            || !self.ask_fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || !distinct(
                self.items
                    .iter()
                    .map(|i| i.child_request.as_uuid())
                    .collect(),
            )
            || !distinct(self.items.iter().map(|i| i.chat.id.as_uuid()).collect())
            || !distinct(
                self.items
                    .iter()
                    .map(|i| i.intent.submission.request.run.as_uuid())
                    .collect(),
            )
            || self.items.iter().any(|i| i.chat.id == self.parent_chat)
        {
            return Err(BotError::Invalid);
        }
        for item in &self.items {
            let s = &item.intent.submission;
            let p = &s.provenance;
            let parent = p.parent.ok_or(BotError::Invalid)?;
            if item.intent.validate().is_err()
                || p.submission.as_uuid() != item.child_request.as_uuid()
                || p.chat_id != item.chat.id.as_uuid()
                || item.turn.attempt.chat_id != Some(item.chat.id)
                || item.turn.user_message.chat_id != item.chat.id
                || item.turn.assistant_message.chat_id != item.chat.id
                || p.ai_attempt_id != Some(item.turn.attempt.id.as_uuid())
                || p.assistant_message_id != Some(item.turn.assistant_message.id.as_uuid())
                || !item.turn.attachment_ids.is_empty()
                || item.chat.branched_from.is_some()
                || p.owner.user_scope != self.owner.user_scope
                || p.owner.device != self.owner.device
                || parent.run != self.parent_run
                || parent.bot != self.parent_bot
                || parent.chat_id != self.parent_chat.as_uuid()
                || parent.child_request != item.child_request
                || s.request.run == self.parent_run
            {
                return Err(BotError::Invalid);
            }
            // The capture is the recipient's own, of this submission.
            if let Some(capture) = &item.capture
                && (capture.owner.user_scope != p.owner.user_scope
                    || capture.owner.device != p.owner.device
                    || capture.bot != p.bot
                    || capture.submission != p.submission
                    || capture.config_revision != p.config_revision
                    || capture.validate(s.workspace).is_err())
            {
                return Err(BotError::Invalid);
            }
        }
        Ok(())
    }
}

/// The most request cards one conversation read returns: the newest ones.
pub const BOT_CHILD_CARD_LIMIT: usize = 60;

/// One retained row of `bot_child_chats`: the ids of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotChildChat {
    pub child_request: ChildRequestId,
    pub parent_bot: BotId,
    pub parent_chat: ChatId,
    pub parent_run: RunId,
    pub parent_assistant_message: MessageId,
    pub ask_key: Uuid,
    pub ordinal: u8,
    pub wait: BotWaitId,
    pub recipient_bot: BotId,
    pub chat: ChatId,
    pub child_run: RunId,
    /// The stage of the recipient's `start` receipt. A conversation whose
    /// receipt was never accepted is listed nowhere.
    pub stage: BotReceiptStage,
    pub created_at_ms: i64,
    /// The stable code a refused ask was answered with, on every row of that
    /// ask, so the same key answers the same refusal later. Never a message.
    pub refusal: Option<String>,
    /// The item the refusal named, when it named one.
    pub refusal_ordinal: Option<u8>,
}
impl BotChildChat {
    /// Whether the delegate admitted this request. Only then is its
    /// conversation shown.
    pub fn admitted(&self) -> bool {
        matches!(
            self.stage,
            BotReceiptStage::Accepted | BotReceiptStage::Completed
        )
    }
}

/// What preparing an ask answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotChildAskPrepared {
    /// Written now: the conversations, one `start` receipt per recipient at
    /// `prepared`, and the request rows.
    Prepared(Vec<BotChildChat>),
    /// The same key and the same list were prepared before: the stored ids
    /// and each receipt's current stage. Nothing was written.
    Retained(Vec<BotChildChat>),
    /// Nothing was written.
    Refused(BotChildAskRefusal),
}

// ---- what a conversation view reads -----------------------------------------

/// Where the child of a request is, read from the child's own rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotChildProgress {
    Queued,
    Running,
    NeedsYou,
    Finished,
}

/// How a settled request reads. Derived at read time: `timed_out` and
/// `parent_stopped` from who settled it; the rest from how the child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotChildOutcome {
    Completed,
    Failed,
    NeededYou,
    Canceled,
    TimedOut,
    ParentStopped,
}
impl BotChildOutcome {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::NeededYou => "needed_you",
            Self::Canceled => "canceled",
            Self::TimedOut => "timed_out",
            Self::ParentStopped => "parent_stopped",
        }
    }
    /// Only a completed request hands anything over.
    pub const fn hands_over(self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// What a request handed over, as a count. Never a title.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BotChildResult {
    /// The request is open, or it settled without completing.
    None,
    /// The child completed and saved this many pages (at most 20 are read).
    Saved { pages: u8 },
    /// The child completed and saved no page.
    NoPageSaved,
}

/// One request card of a parent conversation. Host-fed display data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotChildRequestCard {
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub child_request_id: ChildRequestId,
    /// The assistant message of the parent the card is drawn under.
    pub parent_assistant_message_id: MessageId,
    pub recipient_bot_id: BotId,
    /// The recipient's current name.
    pub recipient_name: String,
    pub child_chat_id: ChatId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub state: ChildRequestState,
    pub child: BotChildProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotChildOutcome>,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub clock: ChildClock,
    pub result: BotChildResult,
    /// The child has an approval or a question open for the person.
    pub needs_you: bool,
}

/// The origin line of a child conversation: who asked for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotChildOrigin {
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub child_request_id: ChildRequestId,
    pub parent_bot_id: BotId,
    /// The asking Bot's current name.
    pub parent_name: String,
    pub parent_chat_id: ChatId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub state: ChildRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotChildOutcome>,
}

/// What a waiting conversation waits on: the Bots it asked whose requests are
/// still open, at most three, and whether its own Bot is paused (a paused
/// Bot's wait has no time limit and is shown as paused).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotActivityWaiting {
    #[cfg_attr(feature = "schema", schemars(length(max = 3)))]
    pub on: Vec<BotId>,
    pub paused: bool,
}

/// A parent conversation with requests the AI owner has not seen settle, as
/// ai.db alone knows it. The worker confirms each against the delegate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotChildWaitingChat {
    pub bot: BotId,
    pub chat: ChatId,
    pub parent_run: RunId,
    pub paused: bool,
    /// Recipients in ordinal order, each once.
    pub recipients: Vec<BotId>,
    pub requests: Vec<ChildRequestId>,
}

#[cfg(test)]
mod tests;
