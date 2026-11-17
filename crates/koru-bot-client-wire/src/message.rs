// SPDX-License-Identifier: MPL-2.0

//! Every message of Bot client protocol 1.
//!
//! Each is one JSON object with a `"type"`, snake_case. A reader ignores a
//! field it does not know; a `type` it does not know is a protocol error. A
//! sender uses a field or a message only when its capability was negotiated
//! ([`crate::negotiate`]).

use std::fmt;

use koru_ai_core::bots::DeviceId;
use koru_ai_core::{ChatId, InferenceAttemptId};
use koru_ai_wire::StreamEvent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::command::{ClientCommand, ClientQuery};
use crate::json::JsonObject;
use crate::record::Record;

/// One frame's content.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireMessage {
    ClientHello(ClientHello),
    OwnerHello(OwnerHello),
    Refused(Refused),
    Subscribe(Subscribe),
    Ack(Ack),
    Gap(Gap),
    Changes(Changes),
    Keepalive(Keepalive),
    Watch(Watch),
    Unwatch(Unwatch),
    Live(Live),
    LiveEnd(LiveEnd),
    Command(Command),
    Receipt(Receipt),
    Query(Query),
    Answer(Answer),
}

impl WireMessage {
    /// Every `type` this build reads.
    pub const TYPES: [&'static str; 16] = [
        "client_hello",
        "owner_hello",
        "refused",
        "subscribe",
        "ack",
        "gap",
        "changes",
        "keepalive",
        "watch",
        "unwatch",
        "live",
        "live_end",
        "command",
        "receipt",
        "query",
        "answer",
    ];

    pub fn knows_type(name: &str) -> bool {
        Self::TYPES.contains(&name)
    }

    /// The message's `type`.
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::ClientHello(_) => "client_hello",
            Self::OwnerHello(_) => "owner_hello",
            Self::Refused(_) => "refused",
            Self::Subscribe(_) => "subscribe",
            Self::Ack(_) => "ack",
            Self::Gap(_) => "gap",
            Self::Changes(_) => "changes",
            Self::Keepalive(_) => "keepalive",
            Self::Watch(_) => "watch",
            Self::Unwatch(_) => "unwatch",
            Self::Live(_) => "live",
            Self::LiveEnd(_) => "live_end",
            Self::Command(_) => "command",
            Self::Receipt(_) => "receipt",
            Self::Query(_) => "query",
            Self::Answer(_) => "answer",
        }
    }

    /// False for a message that holds a kind this build reads and never
    /// writes: a command or a query of a kind it does not know.
    pub fn sendable(&self) -> bool {
        match self {
            Self::Command(command) => !matches!(command.command, ClientCommand::Unsupported),
            Self::Query(query) => !matches!(query.query, ClientQuery::Unsupported),
            _ => true,
        }
    }
}

impl fmt::Debug for WireMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientHello(message) => message.fmt(formatter),
            Self::OwnerHello(message) => message.fmt(formatter),
            Self::Refused(message) => message.fmt(formatter),
            Self::Subscribe(message) => message.fmt(formatter),
            Self::Ack(message) => message.fmt(formatter),
            Self::Gap(message) => message.fmt(formatter),
            Self::Changes(message) => message.fmt(formatter),
            Self::Keepalive(message) => message.fmt(formatter),
            Self::Watch(message) => message.fmt(formatter),
            Self::Unwatch(message) => message.fmt(formatter),
            Self::Live(message) => message.fmt(formatter),
            Self::LiveEnd(message) => message.fmt(formatter),
            Self::Command(message) => message.fmt(formatter),
            Self::Receipt(message) => message.fmt(formatter),
            Self::Query(message) => message.fmt(formatter),
            Self::Answer(message) => message.fmt(formatter),
        }
    }
}

/// What a stream is for. One role per stream, named in its hello.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Feed,
    Command,
    Live,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Command => "command",
            Self::Live => "live",
        }
    }
}

/// The first message of every stream, client to owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub min_version: u32,
    pub max_version: u32,
    pub capabilities: Vec<String>,
    pub role: Role,
    /// Must equal the carrier's authenticated principal.
    pub client_device: DeviceId,
    /// At most 64 bytes. For logs only.
    pub client_build: String,
}

/// The owner's answer to a hello it accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerHello {
    pub version: u32,
    /// The intersection of what the client sent and what the owner offers
    /// this peer.
    pub capabilities: Vec<String>,
    pub owner_device: DeviceId,
    /// 32 hex characters.
    pub feed_id: String,
    /// 32 hex characters: this run of the owner's worker.
    pub boot_id: String,
    /// The feed's newest `seq`.
    pub head: u64,
    pub replica_format: u32,
    /// An open set. A client ignores a value it does not know; an unknown
    /// scope is never an error.
    pub scopes: Vec<String>,
    pub limits: Limits,
    pub owner_time_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_frame_bytes: u32,
    pub max_page_entries: u32,
    pub max_page_bytes: u32,
    pub feed_window: u32,
    pub live_buffer_events: u32,
    pub max_inflight_commands: u32,
}

impl Limits {
    /// The limits an owner of this build announces.
    pub const fn this_build() -> Self {
        Self {
            max_frame_bytes: crate::MAX_FRAME_BYTES as u32,
            max_page_entries: crate::limits::MAX_PAGE_ENTRIES,
            max_page_bytes: crate::limits::MAX_PAGE_BYTES,
            feed_window: crate::limits::FEED_WINDOW,
            live_buffer_events: crate::limits::LIVE_BUFFER_EVENTS,
            max_inflight_commands: crate::limits::MAX_INFLIGHT_COMMANDS,
        }
    }
}

/// The owner declines a stream or a message. Always written by the engine;
/// the carrier writes no frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// One of [`refusal`], or a reason a later build defines.
    pub reason: String,
    /// With `unsupported_version`: the owner's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_version: Option<u32>,
}

impl Refused {
    pub fn new(reason: &str) -> Self {
        Self {
            reason: reason.to_owned(),
            min_version: None,
            max_version: None,
        }
    }

    /// No common protocol version; carries this build's range.
    pub fn unsupported_version() -> Self {
        Self {
            reason: refusal::UNSUPPORTED_VERSION.to_owned(),
            min_version: Some(crate::MIN_VERSION),
            max_version: Some(crate::MAX_VERSION),
        }
    }
}

/// The reasons a `refused` of this build carries.
pub mod refusal {
    /// No common protocol version. Carries the owner's range.
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// `client_device` is not the authenticated device.
    pub const PRINCIPAL_MISMATCH: &str = "principal_mismatch";
    /// The admission port does not name this peer as a client.
    pub const NOT_OWN_DEVICE: &str = "not_own_device";
    /// A message named an `owner_device` that is not this computer.
    pub const WRONG_OWNER: &str = "wrong_owner";
    /// A limit is reached.
    pub const BUSY: &str = "busy";
    /// The AI worker or its Bot authority is not ready. Retry later.
    pub const BOTS_UNAVAILABLE: &str = "bots_unavailable";
    /// A message outside the negotiated capabilities.
    pub const UNSUPPORTED_MESSAGE: &str = "unsupported_message";
    /// A malformed frame, a message in the wrong role, or a frame too large.
    pub const PROTOCOL_ERROR: &str = "protocol_error";

    pub const ALL: [&str; 8] = [
        UNSUPPORTED_VERSION,
        PRINCIPAL_MISMATCH,
        NOT_OWN_DEVICE,
        WRONG_OWNER,
        BUSY,
        BOTS_UNAVAILABLE,
        UNSUPPORTED_MESSAGE,
        PROTOCOL_ERROR,
    ];
}

/// Feed role, client to owner. All three of `feed_id`, `boot_id` and
/// `cursor` null: the first time, or the client asks for a reset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    pub owner_device: DeviceId,
    pub feed_id: Option<String>,
    pub boot_id: Option<String>,
    pub cursor: Option<u64>,
}

impl Subscribe {
    /// The first subscription, or a request for a reset.
    pub fn fresh(owner_device: DeviceId) -> Self {
        Self {
            owner_device,
            feed_id: None,
            boot_id: None,
            cursor: None,
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.feed_id.is_none() && self.boot_id.is_none() && self.cursor.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    FirstSubscribe,
    FeedReplaced,
    FeedRewound,
    ClientRequested,
}

/// Feed role, owner to client: the cursor was not accepted, a reset follows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub owner_device: DeviceId,
    pub reason: GapReason,
    pub head: u64,
}

/// Markers that are not removed and not withheld, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveCounts {
    pub bots: u32,
    pub chats: u32,
    pub messages: u32,
}

/// Feed role, owner to client: one page.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    pub owner_device: DeviceId,
    pub feed_id: String,
    pub boot_id: String,
    pub from_cursor: u64,
    pub to_cursor: u64,
    /// True on every page of a reset.
    pub reset: bool,
    pub caught_up: bool,
    pub head: u64,
    /// Present when `caught_up`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveCounts>,
    /// Present when `caught_up` (D22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld_bots: Option<u32>,
    pub entries: Vec<Entry>,
}

impl fmt::Debug for Changes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Changes")
            .field("owner_device", &self.owner_device)
            .field("from_cursor", &self.from_cursor)
            .field("to_cursor", &self.to_cursor)
            .field("reset", &self.reset)
            .field("caught_up", &self.caught_up)
            .field("head", &self.head)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

/// One change of one subject: its record as it is now, or its removal.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// `from_cursor < seq <= to_cursor`, ascending within the page.
    pub seq: u64,
    /// One of [`crate::entry_kind`]. A kind is sent only when the capability
    /// that defines it was negotiated.
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    pub removed: bool,
    /// Replica format 1. Absent when `removed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Record>,
}

impl Entry {
    /// The subject as it is now.
    pub fn present(seq: u64, id: String, record: Record) -> Self {
        Self {
            seq,
            kind: record.kind().to_owned(),
            id,
            bot_id: record.bot_id(),
            chat_id: record.chat_id(),
            removed: false,
            record: Some(record),
        }
    }

    /// The subject is gone, or is withheld from this client.
    pub fn removed(
        seq: u64,
        kind: &str,
        id: String,
        bot_id: Option<String>,
        chat_id: Option<String>,
    ) -> Self {
        Self {
            seq,
            kind: kind.to_owned(),
            id,
            bot_id,
            chat_id,
            removed: true,
            record: None,
        }
    }

    /// The entry's size as JSON, which is what a page's byte budget counts.
    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Entry")
            .field("seq", &self.seq)
            .field("kind", &self.kind)
            .field("removed", &self.removed)
            .finish_non_exhaustive()
    }
}

/// Feed role, client to owner: every page up to `cursor` is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub cursor: u64,
}

/// Feed role, owner to client, on an idle feed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keepalive {}

/// Live role, client to owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub owner_device: DeviceId,
    pub chat_id: ChatId,
}

/// Live role, client to owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unwatch {
    pub chat_id: ChatId,
}

/// Live role, owner to client: one event of one attempt. Not saved, no
/// cursor; `event.sequence` is per attempt and passed through.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Live {
    pub chat_id: ChatId,
    pub attempt_id: InferenceAttemptId,
    pub event: StreamEvent,
}

impl fmt::Debug for Live {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Live")
            .field("chat_id", &self.chat_id)
            .field("attempt_id", &self.attempt_id)
            .field("sequence", &self.event.sequence)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveEndReason {
    /// The attempt ended.
    Terminal,
    /// The watch's buffer was full.
    Dropped,
    /// The client unwatched, or the stream is closing.
    Closed,
    /// No such conversation for this owner.
    NotFound,
}

/// Live role, owner to client: a watch ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEnd {
    pub chat_id: ChatId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<InferenceAttemptId>,
    pub reason: LiveEndReason,
}

/// Command role, client to owner: one intent.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    /// Minted by the client, once per intent. A re-send is byte-identical.
    pub request_id: Uuid,
    pub owner_device: DeviceId,
    pub command: ClientCommand,
}

impl fmt::Debug for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Command")
            .field("request_id", &self.request_id)
            .field("owner_device", &self.owner_device)
            .field("kind", &self.command.kind())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    /// Durable and not finished (a pause waiting for its delegate).
    Accepted,
    Refused,
    /// The owner recorded the command, stopped before it recorded an
    /// outcome, and cannot tell. Final: the command is not applied again.
    Unknown,
}

/// A command's or a query's failure: a stable code, safe developer-facing
/// detail, and whether sending it again may help.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl fmt::Debug for WireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireFailure")
            .field("code", &self.code)
            .field("retryable", &self.retryable)
            .finish_non_exhaustive()
    }
}

/// Command role, owner to client: the one answer to a command.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub request_id: Uuid,
    pub outcome: Outcome,
    /// The existing public response JSON of the local command. First answer
    /// only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<WireFailure>,
    /// Answered from the owner's durable receipt.
    pub replayed: bool,
    /// The feed's head after the command committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_cursor: Option<u64>,
}

impl fmt::Debug for Receipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Receipt")
            .field("request_id", &self.request_id)
            .field("outcome", &self.outcome)
            .field("failure", &self.failure)
            .field("replayed", &self.replayed)
            .field("feed_cursor", &self.feed_cursor)
            .finish_non_exhaustive()
    }
}

/// Command role, client to owner: a read. It changes nothing.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// At most 64 bytes.
    pub query_id: String,
    pub owner_device: DeviceId,
    pub query: ClientQuery,
}

impl fmt::Debug for Query {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Query")
            .field("owner_device", &self.owner_device)
            .field("kind", &self.query.kind())
            .finish_non_exhaustive()
    }
}

/// Command role, owner to client: the answer to a query.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub query_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<WireFailure>,
}

impl fmt::Debug for Answer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Answer")
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}
