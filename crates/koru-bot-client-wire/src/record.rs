// SPDX-License-Identifier: MPL-2.0

//! Replica payload format 1: what a feed entry carries.
//!
//! A record is the JSON the owner's local public API already returns for the
//! subject, wrapped with a `format` number and the few facts a copy needs
//! that the public shape does not hold.
//!
//! **Not copied in format 1:** attachment bytes and names (only the count),
//! pinned documents, a chat's tool mode and workspace grants, and a Bot's
//! historical Run references. The `bot` record carries no team state: a
//! client derives "paused with the team" from the `team_pause` record it
//! holds for that owner.

use std::fmt;

use koru_ai_core::bots::{Bot, BotChatPurpose, BotId, RunId, TeamPauseView};
use koru_ai_core::{
    AgentNotice, Chat, FeedbackEvent, Message, MessageBlock, ToolActivity, WorkspaceId,
};
use koru_ai_wire::{MessageSources, MessageUsage};
use serde::{Deserialize, Serialize};

use crate::negotiate::entry_kind;

/// The replica payload format this build writes and reads.
pub const REPLICA_FORMAT: u32 = 1;

/// The largest record, as JSON bytes. A message record over it is sent with
/// `body_omitted: true` ([`MessageRecord::fitted`]). A page with one record
/// this large exceeds the page byte limit and still fits a frame.
pub const MAX_RECORD_BYTES: usize = 786_432;

/// A Bot as the owner's `read` answers it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecord {
    pub format: u32,
    pub bot: Bot,
}

impl BotRecord {
    pub fn new(bot: Bot) -> Self {
        Self {
            format: REPLICA_FORMAT,
            bot,
        }
    }
}

/// The two values the owner computes for a conversation's activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatActivity {
    pub running: bool,
    pub needs_you: bool,
}

/// One conversation of a Bot. A conversation whose `chat.private` is true is
/// never sent.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub format: u32,
    pub chat: Chat,
    pub bot_id: BotId,
    pub purpose: BotChatPurpose,
    pub trashed: bool,
    pub activity: ChatActivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_workspace_id: Option<WorkspaceId>,
}

/// One message with everything the conversation shows beside it.
///
/// A pending message is sent with an empty `markdown` and no blocks: live
/// text is not saved (D13). `tool_activities` are the public projection.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub format: u32,
    pub message: Message,
    pub blocks: Vec<MessageBlock>,
    pub tool_activities: Vec<ToolActivity>,
    pub notices: Vec<AgentNotice>,
    pub feedback: Vec<FeedbackEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<MessageUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<MessageSources>,
    /// How many attachments the message has. Their bytes and names are not
    /// copied.
    pub attachment_count: u32,
    /// True when the whole record would exceed [`MAX_RECORD_BYTES`]: the
    /// text and what was shown beside it are left out.
    pub body_omitted: bool,
}

impl MessageRecord {
    /// The record's size as JSON.
    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }

    /// The record as it may be sent: unchanged when it fits
    /// [`MAX_RECORD_BYTES`], otherwise with `body_omitted` and without its
    /// body. The text and its blocks go first; the tool activity, notices,
    /// feedback and sources go only if the record is still too large.
    pub fn fitted(mut self) -> Self {
        if self.encoded_len() <= MAX_RECORD_BYTES {
            return self;
        }
        self.body_omitted = true;
        self.message.markdown.clear();
        self.blocks.clear();
        if self.encoded_len() <= MAX_RECORD_BYTES {
            return self;
        }
        self.tool_activities.clear();
        self.notices.clear();
        self.feedback.clear();
        self.sources = None;
        self
    }
}

/// The owner's team pause.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamPauseRecord {
    pub format: u32,
    pub team: TeamPauseView,
}

impl TeamPauseRecord {
    pub fn new(team: TeamPauseView) -> Self {
        Self {
            format: REPLICA_FORMAT,
            team,
        }
    }
}

/// The record of one entry. On the wire it is the bare record object; which
/// one it is follows from the member only that record has (`bot`, `chat`,
/// `message`, `team`), and must agree with the entry's `kind`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Record {
    Bot(Box<BotRecord>),
    Chat(Box<ChatRecord>),
    Message(Box<MessageRecord>),
    TeamPause(Box<TeamPauseRecord>),
}

impl Record {
    /// The entry kind this record belongs to.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Bot(_) => entry_kind::BOT,
            Self::Chat(_) => entry_kind::CHAT,
            Self::Message(_) => entry_kind::MESSAGE,
            Self::TeamPause(_) => entry_kind::TEAM_PAUSE,
        }
    }

    pub const fn format(&self) -> u32 {
        match self {
            Self::Bot(record) => record.format,
            Self::Chat(record) => record.format,
            Self::Message(record) => record.format,
            Self::TeamPause(record) => record.format,
        }
    }

    /// The Bot the subject belongs to, where the record itself says so. A
    /// message record does not: its entry carries `bot_id`.
    pub fn bot_id(&self) -> Option<String> {
        match self {
            Self::Chat(record) => Some(record.bot_id.to_string()),
            Self::Bot(_) | Self::Message(_) | Self::TeamPause(_) => None,
        }
    }

    /// The conversation the subject belongs to, where the record says so.
    pub fn chat_id(&self) -> Option<String> {
        match self {
            Self::Message(record) => Some(record.message.chat_id.to_string()),
            Self::Bot(_) | Self::Chat(_) | Self::TeamPause(_) => None,
        }
    }

    /// The record's size as JSON.
    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

impl From<BotRecord> for Record {
    fn from(record: BotRecord) -> Self {
        Self::Bot(Box::new(record))
    }
}

impl From<ChatRecord> for Record {
    fn from(record: ChatRecord) -> Self {
        Self::Chat(Box::new(record))
    }
}

impl From<MessageRecord> for Record {
    fn from(record: MessageRecord) -> Self {
        Self::Message(Box::new(record))
    }
}

impl From<TeamPauseRecord> for Record {
    fn from(record: TeamPauseRecord) -> Self {
        Self::TeamPause(Box::new(record))
    }
}

macro_rules! redacted_debug {
    ($name:ident) => {
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("format", &self.format)
                    .finish_non_exhaustive()
            }
        }
    };
}

redacted_debug!(BotRecord);
redacted_debug!(ChatRecord);
redacted_debug!(MessageRecord);
redacted_debug!(TeamPauseRecord);

impl fmt::Debug for Record {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Record({}, format {})",
            self.kind(),
            self.format()
        )
    }
}
