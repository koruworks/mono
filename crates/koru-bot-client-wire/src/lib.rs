// SPDX-License-Identifier: MPL-2.0

//! Bot client protocol, version 1, and replica payload format 1 (M8).
//!
//! A **linked device** (the client) reads and controls the Bots that run on
//! another of the person's computers (the owner) with the messages in this
//! crate. Three stream roles share one frame and one hello:
//!
//! - **feed**: the owner's committed changes, read by cursor ([`Subscribe`],
//!   [`Changes`], [`Ack`], [`Gap`], [`Keepalive`]);
//! - **command**: one intent at a time, each answered by a durable receipt
//!   ([`Command`], [`Receipt`], [`Query`], [`Answer`]);
//! - **live**: the text of a reply while it is written, never saved
//!   ([`Watch`], [`Unwatch`], [`Live`], [`LiveEnd`]).
//!
//! **What is here:** every message, the records a feed entry carries
//! ([`record`]), the frame ([`frame`]), and the pure rules both ends apply to
//! a hello and to each message after it ([`negotiate`]). The golden bytes of
//! every message and record are the files under `vectors/`.
//!
//! **What is not here:** a socket, a carrier, a trust record, a database or a
//! timer. Nothing in this crate knows whether it rides the P2P carrier
//! (ALPN [`PROTOCOL_TOKEN`]) or another one. The engine on the owner and the
//! client's worker write and read every frame; the carrier writes none.
//!
//! **Privacy.** A frame holds what a Bot and a person wrote. No `Debug` and
//! no error in this crate prints a message's text, a title, a command's
//! payload or a response: they print ids, kinds, counts and sizes.

pub mod command;
pub mod frame;
mod json;
pub mod message;
pub mod negotiate;
pub mod record;

#[cfg(test)]
mod tests;

pub use command::{ClientCommand, ClientQuery};
pub use frame::{
    FRAME_HEADER_BYTES, FrameDecoder, MAX_FRAME_BYTES, WireError, decode_body, encode, encode_body,
};
pub use json::JsonObject;
pub use message::{
    Ack, Answer, Changes, ClientHello, Command, Entry, Gap, GapReason, Keepalive, Limits, Live,
    LiveCounts, LiveEnd, LiveEndReason, Outcome, OwnerHello, Query, Receipt, Refused, Role,
    Subscribe, Unwatch, Watch, WireFailure, WireMessage, refusal,
};
pub use negotiate::{
    BUILT_CAPABILITIES, CapabilityDef, CapabilityState, MAX_VERSION, MIN_VERSION, Negotiated,
    PageBuilder, PageExpectation, RESERVED_CAPABILITIES, Violation, capability,
    client_capabilities, common_version, entry_kind, is_feed_token, owner_capabilities, scope,
};
pub use record::{
    BotRecord, ChatActivity, ChatRecord, MAX_RECORD_BYTES, MessageRecord, REPLICA_FORMAT, Record,
    TeamPauseRecord,
};

/// The protocol token of the P2P carrier, which is also its ALPN. The number
/// changes only when the framing changes; a new message is a capability.
pub const PROTOCOL_TOKEN: &str = "koru/bots/1";

/// The limits an owner of this build announces in its hello and holds a
/// client to (contract 2.4 and 2.5).
pub mod limits {
    /// One page carries at most this many entries.
    pub const MAX_PAGE_ENTRIES: u32 = 64;
    /// One page carries at most this many bytes of entries, except that a
    /// page always holds at least one entry.
    pub const MAX_PAGE_BYTES: u32 = 524_288;
    /// Pages the owner sends before it waits for an [`crate::Ack`].
    pub const FEED_WINDOW: u32 = 2;
    /// Live events kept per watch before the watch ends `dropped`.
    pub const LIVE_BUFFER_EVENTS: u32 = 64;
    /// Commands one client may have in flight across its streams.
    pub const MAX_INFLIGHT_COMMANDS: u32 = 4;
    /// Watches one live stream may hold.
    pub const MAX_WATCHES_PER_STREAM: u32 = 4;
    /// Open streams per connection.
    pub const MAX_STREAMS_PER_CONNECTION: u32 = 4;
    /// Linked devices served at once (D18); one more is refused `busy`.
    pub const MAX_PEER_DEVICES: u32 = 8;
    /// A stream that has not sent its hello after this long is closed.
    pub const HELLO_TIMEOUT_MS: u64 = 10_000;
    /// An idle command stream is closed after this long.
    pub const COMMAND_IDLE_TIMEOUT_MS: u64 = 120_000;
    /// A page left unacknowledged this long disconnects the client.
    pub const ACK_TIMEOUT_MS: u64 = 60_000;
    /// The owner's keepalive interval on an idle feed.
    pub const KEEPALIVE_INTERVAL_MS: u64 = 30_000;
    /// A client that hears nothing on a feed for this long treats it as closed.
    pub const FEED_SILENCE_TIMEOUT_MS: u64 = 90_000;
    /// The longest `client_build`, in bytes.
    pub const MAX_CLIENT_BUILD_BYTES: usize = 64;
    /// The most capability names a hello may carry.
    pub const MAX_CAPABILITIES: usize = 64;
    /// The longest `query_id`, in bytes.
    pub const MAX_QUERY_ID_BYTES: usize = 64;
}
