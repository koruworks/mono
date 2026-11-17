// SPDX-License-Identifier: MPL-2.0

//! The rules both ends apply to a hello and to every message after it.
//!
//! All of it is pure: no clock, no carrier, no store. The owner's engine and
//! the client's worker call these and act on the answer.
//!
//! **A later milestone adds a capability, not a version.** One capability has
//! one meaning and one scope on every kind of owner. An entry kind is sent
//! only when the capability that defines it was negotiated.

use std::collections::BTreeSet;
use std::fmt;

use koru_ai_core::bots::DeviceId;

use crate::limits;
use crate::message::{
    Changes, ClientHello, Entry, Limits, OwnerHello, Refused, Role, WireMessage, refusal,
};
use crate::record::REPLICA_FORMAT;

/// The oldest protocol version this build speaks.
pub const MIN_VERSION: u32 = 1;
/// The newest protocol version this build speaks.
pub const MAX_VERSION: u32 = 1;

/// The scopes an M8 owner grants. The set is open: a later owner may send
/// others, and a client ignores a value it does not know.
pub mod scope {
    pub const READ: &str = "read";
    pub const CONTROL: &str = "control";
    pub const CONFIGURE: &str = "configure";
}

/// Capability names.
pub mod capability {
    /// The feed role, replica format 1, the four entry kinds. Required.
    pub const FEED: &str = "feed.1";
    /// `send_turn`, `retry_turn`, `cancel_attempt`, `steer`, `create_chat`.
    pub const CMD_CHAT: &str = "cmd.chat.1";
    /// `pause_bot`, `resume_bot`.
    pub const CMD_PAUSE: &str = "cmd.pause.1";
    /// `pause_team`, `resume_team`.
    pub const CMD_TEAM_PAUSE: &str = "cmd.team_pause.1";
    /// `answer_decision`.
    pub const CMD_DECISIONS: &str = "cmd.decisions.1";
    /// `update_bot`.
    pub const CMD_CONFIG: &str = "cmd.config.1";
    /// `read_run` and `read_operation`.
    pub const QUERY_RUN: &str = "query.run.1";
    /// The live role carrying `StreamEvent` of AI stream version 1.
    pub const LIVE_AI_STREAM: &str = "live.ai_stream.1";
    /// Reserved: Routine on, off, timing, Test, and the Routine reads.
    pub const CMD_ROUTINES: &str = "cmd.routines.1";
    /// Reserved: Archive, Trash, Restore.
    pub const CMD_LIFECYCLE: &str = "cmd.lifecycle.1";
    /// Reserved for an owner with no screen: creating a Bot on the owner.
    pub const CMD_CREATE: &str = "cmd.create.1";
    /// Reserved for an owner with no screen: access changes.
    pub const CMD_ACCESS: &str = "cmd.access.1";
    /// Reserved for an owner with no screen: Role, Knowledge and Memory.
    pub const CMD_CONTEXT: &str = "cmd.context.1";
    /// Reserved for an owner with no screen: creating and editing a Routine.
    pub const CMD_ROUTINES_ADMIN: &str = "cmd.routines.admin.1";
    /// Reserved for an owner with no screen: its Cloud connection.
    pub const CMD_CLOUD: &str = "cmd.cloud.1";
    /// Reserved for an owner with no screen: reads about it.
    pub const QUERY_SERVER: &str = "query.server.1";
    /// Reserved: Routine entry kinds.
    pub const FEED_ROUTINES: &str = "feed.routines.1";
    /// Reserved: Run entry kinds.
    pub const FEED_RUNS: &str = "feed.runs.1";
}

/// Entry kinds. All four are defined by `feed.1`.
pub mod entry_kind {
    pub const BOT: &str = "bot";
    pub const CHAT: &str = "chat";
    pub const MESSAGE: &str = "message";
    pub const TEAM_PAUSE: &str = "team_pause";

    /// The capability that defines `kind`, or `None` for a kind this build
    /// does not know (which it can therefore never have negotiated).
    pub fn capability(kind: &str) -> Option<&'static str> {
        match kind {
            BOT | CHAT | MESSAGE | TEAM_PAUSE => Some(super::capability::FEED),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityState {
    /// Built in this build: a client asks for it, an owner may offer it.
    Built,
    /// The name, its meaning and its scope are fixed; nothing implements it.
    /// Never offered and never negotiated by this build.
    Reserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityDef {
    pub name: &'static str,
    /// The scope a peer must hold. `None` for a reserved name whose scope a
    /// later milestone defines.
    pub scope: Option<&'static str>,
    pub state: CapabilityState,
}

const fn built(name: &'static str, scope: &'static str) -> CapabilityDef {
    CapabilityDef {
        name,
        scope: Some(scope),
        state: CapabilityState::Built,
    }
}

const fn reserved(name: &'static str, scope: Option<&'static str>) -> CapabilityDef {
    CapabilityDef {
        name,
        scope,
        state: CapabilityState::Reserved,
    }
}

/// The capabilities of this build, in the order a hello lists them.
pub const BUILT_CAPABILITIES: [CapabilityDef; 8] = [
    built(capability::FEED, scope::READ),
    built(capability::CMD_CHAT, scope::CONTROL),
    built(capability::CMD_PAUSE, scope::CONTROL),
    built(capability::CMD_TEAM_PAUSE, scope::CONTROL),
    built(capability::CMD_DECISIONS, scope::CONTROL),
    built(capability::CMD_CONFIG, scope::CONFIGURE),
    built(capability::QUERY_RUN, scope::READ),
    built(capability::LIVE_AI_STREAM, scope::READ),
];

/// Names that keep one meaning and are not built. An M8 owner never
/// advertises one.
pub const RESERVED_CAPABILITIES: [CapabilityDef; 10] = [
    reserved(capability::CMD_ROUTINES, Some(scope::CONFIGURE)),
    reserved(capability::CMD_LIFECYCLE, Some(scope::CONFIGURE)),
    reserved(capability::CMD_CREATE, None),
    reserved(capability::CMD_ACCESS, None),
    reserved(capability::CMD_CONTEXT, None),
    reserved(capability::CMD_ROUTINES_ADMIN, None),
    reserved(capability::CMD_CLOUD, None),
    reserved(capability::QUERY_SERVER, None),
    reserved(capability::FEED_ROUTINES, None),
    reserved(capability::FEED_RUNS, None),
];

/// What a client of this build asks for.
pub fn client_capabilities() -> Vec<String> {
    BUILT_CAPABILITIES
        .iter()
        .map(|capability| capability.name.to_owned())
        .collect()
}

/// What an owner of this build may offer a peer that holds `scopes`: every
/// built capability whose scope the peer holds. An unknown scope value adds
/// nothing and is not an error.
pub fn owner_capabilities(scopes: &BTreeSet<String>) -> Vec<&'static str> {
    BUILT_CAPABILITIES
        .iter()
        .filter(|capability| capability.scope.is_some_and(|scope| scopes.contains(scope)))
        .map(|capability| capability.name)
        .collect()
}

/// The newest version both ranges hold, or `None`.
pub fn common_version(client_min: u32, client_max: u32) -> Option<u32> {
    if client_min > client_max {
        return None;
    }
    let newest = client_max.min(MAX_VERSION);
    (newest >= client_min.max(MIN_VERSION)).then_some(newest)
}

/// A message that breaks the protocol after the frame decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Violation {
    /// A hello whose own fields are out of bounds.
    BadHello,
    /// A hello where none belongs: a second one, or an owner's from a client.
    UnexpectedHello,
    /// A message the other end sends, not this one.
    WrongDirection,
    /// A message of another stream role.
    WrongRole,
    /// A message or a kind whose capability was not negotiated.
    NotNegotiated,
    /// An entry kind whose capability was not negotiated, which includes
    /// every kind this build does not know.
    EntryKindNotNegotiated,
    /// An entry whose record is missing, unexpected or of another kind.
    RecordMismatch,
    /// A record in a replica format this build does not know.
    UnknownReplicaFormat,
    /// A page that is not the next one: another feed, another start, another
    /// cursor, or `seq` values out of order.
    PageOutOfOrder,
    /// A page over the announced limits.
    PageTooLarge,
    /// The owner answered a hello with something the client did not offer.
    BadAnswer,
    /// An id field that is not the shape the protocol gives it.
    BadId,
}

impl Violation {
    /// The reason an owner's `refused` carries for this violation.
    pub const fn refusal(self) -> &'static str {
        match self {
            Self::NotNegotiated | Self::EntryKindNotNegotiated => refusal::UNSUPPORTED_MESSAGE,
            _ => refusal::PROTOCOL_ERROR,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BadHello => "a hello out of bounds",
            Self::UnexpectedHello => "a hello where none belongs",
            Self::WrongDirection => "a message the other end sends",
            Self::WrongRole => "a message of another stream role",
            Self::NotNegotiated => "a message outside the negotiated capabilities",
            Self::EntryKindNotNegotiated => "an entry kind outside the negotiated capabilities",
            Self::RecordMismatch => "an entry whose record does not match it",
            Self::UnknownReplicaFormat => "a record in an unknown replica format",
            Self::PageOutOfOrder => "a page that is not the next one",
            Self::PageTooLarge => "a page over the announced limits",
            Self::BadAnswer => "an answer to a hello that was not offered",
            Self::BadId => "an id of the wrong shape",
        })
    }
}

impl std::error::Error for Violation {}

/// 32 lowercase hex characters: a `feed_id` or a `boot_id`.
pub fn is_feed_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn hello_in_bounds(hello: &ClientHello) -> bool {
    hello.min_version <= hello.max_version
        && hello.client_build.len() <= limits::MAX_CLIENT_BUILD_BYTES
        && hello.capabilities.len() <= limits::MAX_CAPABILITIES
        && hello
            .capabilities
            .iter()
            .all(|name| !name.is_empty() && name.len() <= 64)
}

/// What one stream agreed on. Built once, from the hello, by each end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub version: u32,
    pub role: Role,
    capabilities: BTreeSet<&'static str>,
    scopes: BTreeSet<String>,
}

impl Negotiated {
    /// **Owner side.** Decide a client's hello. `authenticated` is the
    /// carrier's principal; `peer_scopes` is the admission port's answer for
    /// that peer; `supported` is what this owner has built, a subset of
    /// [`BUILT_CAPABILITIES`] by name (a reserved or unknown name in it is
    /// ignored, so it can never be offered).
    ///
    /// The checks, in order: the hello's own bounds (`protocol_error`), a
    /// common version (`unsupported_version`, with this build's range), the
    /// principal (`principal_mismatch`), then the capabilities: `feed.1` is
    /// required on every stream, and the stream's role needs at least one
    /// capability of its own (`unsupported_message`).
    pub fn owner_side(
        hello: &ClientHello,
        authenticated: &DeviceId,
        peer_scopes: &BTreeSet<String>,
        supported: &[&str],
    ) -> Result<Self, Refused> {
        if !hello_in_bounds(hello) {
            return Err(Refused::new(refusal::PROTOCOL_ERROR));
        }
        let Some(version) = common_version(hello.min_version, hello.max_version) else {
            return Err(Refused::unsupported_version());
        };
        if hello.client_device != *authenticated {
            return Err(Refused::new(refusal::PRINCIPAL_MISMATCH));
        }
        let capabilities: BTreeSet<&'static str> = owner_capabilities(peer_scopes)
            .into_iter()
            .filter(|name| supported.contains(name))
            .filter(|name| hello.capabilities.iter().any(|asked| asked == name))
            .collect();
        let negotiated = Self {
            version,
            role: hello.role,
            capabilities,
            scopes: peer_scopes.clone(),
        };
        if !negotiated.has(capability::FEED) || !negotiated.role_is_served() {
            return Err(Refused::new(refusal::UNSUPPORTED_MESSAGE));
        }
        Ok(negotiated)
    }

    /// **Client side.** Read the owner's answer to the hello this client
    /// `sent`. The version must be one the client offered, every capability
    /// one it asked for and knows, and the replica format one it reads. An
    /// unknown scope value is dropped, never an error.
    pub fn client_side(sent: &ClientHello, answer: &OwnerHello) -> Result<Self, Violation> {
        if answer.version < sent.min_version.max(MIN_VERSION)
            || answer.version > sent.max_version.min(MAX_VERSION)
        {
            return Err(Violation::BadAnswer);
        }
        if answer.replica_format != REPLICA_FORMAT {
            return Err(Violation::UnknownReplicaFormat);
        }
        if !is_feed_token(&answer.feed_id) || !is_feed_token(&answer.boot_id) {
            return Err(Violation::BadId);
        }
        let mut capabilities = BTreeSet::new();
        for name in &answer.capabilities {
            let known = BUILT_CAPABILITIES
                .iter()
                .find(|capability| capability.name == name);
            match known {
                Some(capability) if sent.capabilities.contains(name) => {
                    capabilities.insert(capability.name);
                }
                _ => return Err(Violation::BadAnswer),
            }
        }
        let scopes = answer
            .scopes
            .iter()
            .filter(|scope| {
                matches!(
                    scope.as_str(),
                    scope::READ | scope::CONTROL | scope::CONFIGURE
                )
            })
            .cloned()
            .collect();
        let negotiated = Self {
            version: answer.version,
            role: sent.role,
            capabilities,
            scopes,
        };
        if !negotiated.has(capability::FEED) || !negotiated.role_is_served() {
            return Err(Violation::BadAnswer);
        }
        Ok(negotiated)
    }

    /// The owner's hello for this stream.
    pub fn owner_hello(
        &self,
        owner_device: DeviceId,
        feed_id: String,
        boot_id: String,
        head: u64,
        owner_time_ms: i64,
    ) -> OwnerHello {
        OwnerHello {
            version: self.version,
            capabilities: self.capabilities(),
            owner_device,
            feed_id,
            boot_id,
            head,
            replica_format: REPLICA_FORMAT,
            scopes: self.ordered_scopes(),
            limits: Limits::this_build(),
            owner_time_ms,
        }
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    /// The negotiated capabilities, in the order a hello lists them.
    pub fn capabilities(&self) -> Vec<String> {
        BUILT_CAPABILITIES
            .iter()
            .filter(|capability| self.capabilities.contains(capability.name))
            .map(|capability| capability.name.to_owned())
            .collect()
    }

    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.scopes
    }

    /// The scopes as a hello lists them: `read`, `control`, `configure`,
    /// then any other value in order.
    fn ordered_scopes(&self) -> Vec<String> {
        let known = [scope::READ, scope::CONTROL, scope::CONFIGURE];
        let mut ordered: Vec<String> = known
            .iter()
            .filter(|scope| self.scopes.contains(**scope))
            .map(|scope| (*scope).to_owned())
            .collect();
        ordered.extend(
            self.scopes
                .iter()
                .filter(|scope| !known.contains(&scope.as_str()))
                .cloned(),
        );
        ordered
    }

    fn role_is_served(&self) -> bool {
        match self.role {
            Role::Feed => self.has(capability::FEED),
            Role::Live => self.has(capability::LIVE_AI_STREAM),
            Role::Command => self
                .capabilities
                .iter()
                .any(|name| name.starts_with("cmd.") || name.starts_with("query.")),
        }
    }

    fn in_role(&self, role: Role) -> Result<(), Violation> {
        if self.role == role {
            Ok(())
        } else {
            Err(Violation::WrongRole)
        }
    }

    fn negotiated(&self, capability: Option<&'static str>) -> Result<(), Violation> {
        match capability {
            Some(capability) if !self.has(capability) => Err(Violation::NotNegotiated),
            _ => Ok(()),
        }
    }

    /// **Owner side.** May the client send `message` on this stream, after
    /// its hello? A command or a query of a kind this build does not know
    /// passes: the engine answers it with a receipt or an answer.
    pub fn check_from_client(&self, message: &WireMessage) -> Result<(), Violation> {
        match message {
            WireMessage::ClientHello(_) => Err(Violation::UnexpectedHello),
            WireMessage::Subscribe(_) | WireMessage::Ack(_) => self.in_role(Role::Feed),
            WireMessage::Watch(_) | WireMessage::Unwatch(_) => self.in_role(Role::Live),
            WireMessage::Command(command) => {
                self.in_role(Role::Command)?;
                self.negotiated(command.command.capability())
            }
            WireMessage::Query(query) => {
                self.in_role(Role::Command)?;
                if query.query_id.is_empty() || query.query_id.len() > limits::MAX_QUERY_ID_BYTES {
                    return Err(Violation::BadId);
                }
                self.negotiated(query.query.capability())
            }
            WireMessage::OwnerHello(_)
            | WireMessage::Refused(_)
            | WireMessage::Gap(_)
            | WireMessage::Changes(_)
            | WireMessage::Keepalive(_)
            | WireMessage::Live(_)
            | WireMessage::LiveEnd(_)
            | WireMessage::Receipt(_)
            | WireMessage::Answer(_) => Err(Violation::WrongDirection),
        }
    }

    /// **Client side.** May the owner send `message` on this stream, after
    /// its hello? A `refused` may arrive in any role. A page's entries are
    /// checked too ([`Negotiated::check_entry`]).
    pub fn check_from_owner(&self, message: &WireMessage) -> Result<(), Violation> {
        match message {
            WireMessage::OwnerHello(_) => Err(Violation::UnexpectedHello),
            WireMessage::Refused(_) => Ok(()),
            WireMessage::Gap(_) | WireMessage::Keepalive(_) => self.in_role(Role::Feed),
            WireMessage::Changes(changes) => {
                self.in_role(Role::Feed)?;
                changes
                    .entries
                    .iter()
                    .try_for_each(|entry| self.check_entry(entry))
            }
            WireMessage::Live(_) | WireMessage::LiveEnd(_) => self.in_role(Role::Live),
            WireMessage::Receipt(_) | WireMessage::Answer(_) => self.in_role(Role::Command),
            WireMessage::ClientHello(_)
            | WireMessage::Subscribe(_)
            | WireMessage::Ack(_)
            | WireMessage::Watch(_)
            | WireMessage::Unwatch(_)
            | WireMessage::Command(_)
            | WireMessage::Query(_) => Err(Violation::WrongDirection),
        }
    }

    /// One entry: its kind's capability was negotiated, and its record is
    /// there exactly when it is not removed, is of that kind, and is in a
    /// format this build reads.
    pub fn check_entry(&self, entry: &Entry) -> Result<(), Violation> {
        match entry_kind::capability(&entry.kind) {
            Some(capability) if self.has(capability) => {}
            _ => return Err(Violation::EntryKindNotNegotiated),
        }
        match (&entry.record, entry.removed) {
            (None, true) => Ok(()),
            (Some(record), false) => {
                if record.kind() != entry.kind {
                    Err(Violation::RecordMismatch)
                } else if record.format() != REPLICA_FORMAT {
                    Err(Violation::UnknownReplicaFormat)
                } else {
                    Ok(())
                }
            }
            _ => Err(Violation::RecordMismatch),
        }
    }
}

/// What a client expects of the next page: the feed and start its hello
/// named, and the cursor it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageExpectation<'a> {
    pub owner_device: &'a DeviceId,
    pub feed_id: &'a str,
    pub boot_id: &'a str,
    pub cursor: u64,
}

impl Changes {
    /// The transport checks of one page (contract 4.6). A page that fails is
    /// not applied; the client closes the stream and subscribes again.
    ///
    /// `from_cursor` is the client's cursor (0 on the first page of a
    /// reset), the feed and start are those of the hello, `seq` values
    /// ascend inside `(from_cursor, to_cursor]`, and the counts are present
    /// exactly on a `caught_up` page.
    pub fn check_page(&self, expected: &PageExpectation<'_>) -> Result<(), Violation> {
        if self.owner_device != *expected.owner_device
            || self.feed_id != expected.feed_id
            || self.boot_id != expected.boot_id
            || self.from_cursor != expected.cursor
            || self.to_cursor < self.from_cursor
            || self.head < self.to_cursor
        {
            return Err(Violation::PageOutOfOrder);
        }
        let mut last = self.from_cursor;
        for entry in &self.entries {
            if entry.seq <= last || entry.seq > self.to_cursor {
                return Err(Violation::PageOutOfOrder);
            }
            last = entry.seq;
        }
        if self.caught_up != self.live.is_some() || self.caught_up != self.withheld_bots.is_some() {
            return Err(Violation::RecordMismatch);
        }
        Ok(())
    }

    /// The page is within `limits`: at most `max_page_entries`, and at most
    /// `max_page_bytes` of entries unless it holds exactly one.
    pub fn check_limits(&self, limits: &Limits) -> Result<(), Violation> {
        if self.entries.len() > limits.max_page_entries as usize {
            return Err(Violation::PageTooLarge);
        }
        if self.entries.len() > 1 {
            let bytes: usize = self.entries.iter().map(Entry::encoded_len).sum();
            if bytes > limits.max_page_bytes as usize {
                return Err(Violation::PageTooLarge);
            }
        }
        Ok(())
    }
}

/// Fills one page within the limits: at most `max_page_entries` entries and
/// `max_page_bytes` of them, and always at least one entry, so a single
/// large record still goes out.
#[derive(Debug)]
pub struct PageBuilder {
    limits: Limits,
    entries: Vec<Entry>,
    bytes: usize,
}

impl PageBuilder {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            entries: Vec::new(),
            bytes: 0,
        }
    }

    /// Add `entry` if the page has room for it. `Err` gives the entry back:
    /// the page is full and the entry opens the next one.
    pub fn push(&mut self, entry: Entry) -> Result<(), Entry> {
        let bytes = entry.encoded_len();
        let full = !self.entries.is_empty()
            && (self.entries.len() >= self.limits.max_page_entries as usize
                || self.bytes.saturating_add(bytes) > self.limits.max_page_bytes as usize);
        if full {
            return Err(entry);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.push(entry);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes of the entries added so far.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn finish(self) -> Vec<Entry> {
        self.entries
    }
}
