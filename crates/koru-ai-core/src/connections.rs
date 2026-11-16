// SPDX-License-Identifier: MPL-2.0

//! A person's private connections (M5), as the AI owner keeps them in ai.db.
//!
//! A **connection** is one signed-in account at one vendor, owned by a person
//! on this computer. Its credential lives in the OS credential store; nothing
//! here can spell one. What is here is metadata: ids, digests, closed tokens,
//! revisions, and three pieces of private text the person recognizes (the
//! name they gave, the account label, an item's title) plus the vendor's own
//! item reference. That text stays in ai.db and in the owner's UI. It is
//! redacted from every `Debug` in this module and appears in no error.
//!
//! A value of these types grants nothing. Whether a Bot may use a connection
//! is its policy ([`crate::bots::Bot::connections`]); whether the connection
//! can be used now is read from ai.db at use time.

use crate::bots::{BotOwner, BotRequestId};
pub use koru_delegate_core::{
    CONNECTION_GRANT_LIMIT, ConnectionGrantRef, ConnectionId, ConnectionItemId,
    PrivateOperationKind,
};
use std::collections::BTreeSet;

/// Live (not disconnected) connections one person may hold on one computer.
pub const CONNECTION_LIMIT: usize = 8;
/// Items one connection may hold.
pub const CONNECTION_ITEM_LIMIT: usize = 256;
/// The longest stored item title, in bytes. A longer vendor title is cut at a
/// character boundary by whoever stores it.
pub const CONNECTION_TITLE_BYTES: usize = 512;

/// A 32-byte BLAKE3 digest of an identifier. Never of a secret.
pub type ConnectionDigest = [u8; 32];

/// The provider tokens ai.db admits. This is the one place a vendor is named:
/// the list mirrors the `provider` CHECK of migration 025, and a test compares
/// the two. A token is data; no identifier outside this module names a vendor.
pub mod vendors {
    /// Journey A of the M5 contract (provisional until P5.1 qualifies it).
    pub const QUALIFIED: &[&str] = &["google_drive"];
}

/// One of [`vendors::QUALIFIED`]. Not a secret and not private.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderToken(&'static str);
impl ProviderToken {
    pub fn parse(value: &str) -> Option<Self> {
        vendors::QUALIFIED
            .iter()
            .copied()
            .find(|token| *token == value)
            .map(Self)
    }
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

macro_rules! tokens {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn token(self) -> &'static str {
                match self { $(Self::$variant => $token),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($token => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

tokens!(
    /// `connected` can be used. `needs_sign_in`: the vendor refused the stored
    /// credential; grants are kept and nothing is read until the same account
    /// signs in again. `disconnecting` is the fence set before the vendor
    /// revoke. `disconnected` is a tombstone and is final.
    ConnectionState {
        Connected => "connected",
        NeedsSignIn => "needs_sign_in",
        Disconnecting => "disconnecting",
        Disconnected => "disconnected",
    }
);
tokens!(
    /// What the vendor said when the account was disconnected.
    VendorRevoke {
        Confirmed => "confirmed",
        Unconfirmed => "unconfirmed",
        Unsupported => "unsupported",
    }
);
tokens!(
    /// `changed`: a read found another identity than the one the person
    /// picked. The item is not read again until the person picks it again.
    ConnectionItemState {
        Available => "available",
        Changed => "changed",
    }
);
tokens!(
    /// The idempotent connection commands. Signing in again is a `connect`
    /// whose planned connection is the existing one.
    ConnectionCommandKind {
        Connect => "connect",
        AddItems => "add_items",
        Disconnect => "disconnect",
    }
);
tokens!(
    ConnectionReceiptStage {
        Prepared => "prepared",
        Completed => "completed",
        Failed => "failed",
    }
);
tokens!(
    /// Why a connection command ended without its effect. A closed list; no
    /// vendor text.
    ConnectionFailure {
        Canceled => "canceled",
        TimedOut => "timed_out",
        Interrupted => "interrupted",
        ConsentDenied => "consent_denied",
        WrongAccount => "wrong_account",
        AccountInUse => "account_in_use",
        NameInUse => "name_in_use",
        VendorRejected => "vendor_rejected",
        VendorUnavailable => "vendor_unavailable",
        CredentialStoreUnavailable => "credential_store_unavailable",
        NotConfigured => "not_configured",
        RevisionConflict => "revision_conflict",
        ItemLimit => "item_limit",
    }
);

/// One row of `connections`.
#[derive(Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: ConnectionId,
    pub owner: BotOwner,
    pub provider: ProviderToken,
    pub service_origin: String,
    /// The name the person gave. Fixed for the life of the connection.
    pub name: String,
    pub account_subject_digest: ConnectionDigest,
    /// What the person recognizes, for example an email address. Shown to
    /// the person only.
    pub account_label: String,
    pub scopes: Vec<String>,
    pub credential_slot_digest: ConnectionDigest,
    pub state: ConnectionState,
    pub vendor_revoke: Option<VendorRevoke>,
    pub revision: u64,
    pub connected_at_ms: i64,
    pub refreshed_at_ms: Option<i64>,
    pub disconnected_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("state", &self.state)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
impl Connection {
    /// The grant reference a Bot's policy holds for a read on this connection.
    pub fn read_grant(&self) -> ConnectionGrantRef {
        ConnectionGrantRef {
            connection: self.id,
            operation: PrivateOperationKind::ReadItem,
        }
    }
}

/// One row of `connection_items`: an item the person picked.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionItem {
    pub id: ConnectionItemId,
    pub connection: ConnectionId,
    /// The vendor's own identifier. Private metadata: local only.
    pub item_ref: String,
    pub item_ref_digest: ConnectionDigest,
    /// What the item was when the person picked it (D17).
    pub identity_digest: ConnectionDigest,
    pub title: String,
    pub state: ConnectionItemState,
    pub added_at_ms: i64,
    pub changed_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}
impl std::fmt::Debug for ConnectionItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionItem")
            .field("id", &self.id)
            .field("connection", &self.connection)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// An item as the vendor reported it when the person picked it.
#[derive(Clone, PartialEq, Eq)]
pub struct PickedConnectionItem {
    pub item_ref: String,
    pub item_ref_digest: ConnectionDigest,
    pub identity_digest: ConnectionDigest,
    pub title: String,
}
impl std::fmt::Debug for PickedConnectionItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickedConnectionItem([REDACTED])")
    }
}
impl PickedConnectionItem {
    fn valid(&self) -> bool {
        (1..=1024).contains(&self.item_ref.len())
            && (1..=CONNECTION_TITLE_BYTES).contains(&self.title.len())
    }
}

/// Whether `items` can be stored: each within its bounds, none twice, at most
/// [`CONNECTION_ITEM_LIMIT`].
pub fn valid_picked_items(items: &[PickedConnectionItem]) -> bool {
    let mut seen = BTreeSet::new();
    items.len() <= CONNECTION_ITEM_LIMIT
        && items
            .iter()
            .all(|item| item.valid() && seen.insert(item.item_ref_digest))
}

/// Writer input for a connect that completed at the vendor. The credential is
/// already in the slot `credential_slot_digest` names; it is not here.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectedAccount {
    pub provider: ProviderToken,
    pub service_origin: String,
    pub name: String,
    pub account_subject_digest: ConnectionDigest,
    pub account_label: String,
    pub scopes: Vec<String>,
    pub credential_slot_digest: ConnectionDigest,
    pub items: Vec<PickedConnectionItem>,
}
impl std::fmt::Debug for ConnectedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectedAccount([REDACTED])")
    }
}
impl ConnectedAccount {
    /// The bounds the table's `CHECK`s hold, so a refusal is typed and not an
    /// SQL error.
    pub fn validate(&self) -> Result<(), ConnectionError> {
        if !valid_connection_name(&self.name)
            || !(9..=256).contains(&self.service_origin.len())
            || !self.service_origin.starts_with("https://")
            || !(1..=320).contains(&self.account_label.len())
            || self.scopes.len() > 64
            || self.scopes.iter().any(|scope| scope.is_empty())
            || scopes_json(&self.scopes).is_none()
            || !valid_picked_items(&self.items)
        {
            return Err(ConnectionError::Invalid);
        }
        Ok(())
    }
}

/// A name the person can read back: 1 to 256 bytes, not blank, no control
/// character, no surrounding space.
pub fn valid_connection_name(name: &str) -> bool {
    (1..=256).contains(&name.len()) && name.trim() == name && !name.chars().any(char::is_control)
}

/// The stored encoding of granted scopes, when it fits its column.
pub fn scopes_json(scopes: &[String]) -> Option<String> {
    serde_json::to_string(scopes)
        .ok()
        .filter(|json| (2..=4096).contains(&json.len()))
}

/// One row of `connection_command_receipts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionCommandReceipt {
    pub request: BotRequestId,
    pub owner: BotOwner,
    pub kind: ConnectionCommandKind,
    /// 64 lowercase hex characters over the command's arguments.
    pub fingerprint: String,
    /// The id a connect will use if it completes. The credential slot is
    /// named from it before any connection row exists.
    pub planned_connection: Option<ConnectionId>,
    pub connection: Option<ConnectionId>,
    pub stage: ConnectionReceiptStage,
    pub failure: Option<ConnectionFailure>,
    pub expected_revision: Option<u64>,
    pub result_revision: Option<u64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// What the repository did with a `begin` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionCommandAdmission {
    /// This call wrote the `prepared` receipt.
    Prepared(ConnectionCommandReceipt),
    /// The same request with the same fingerprint was retained earlier, in
    /// whatever stage it has now. Nothing was written.
    Retained(ConnectionCommandReceipt),
}
impl ConnectionCommandAdmission {
    pub fn receipt(&self) -> &ConnectionCommandReceipt {
        match self {
            Self::Prepared(receipt) | Self::Retained(receipt) => receipt,
        }
    }
}

/// Whether a fingerprint has the stored shape.
pub fn valid_connection_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A refusal of a connection read or write. Fixed codes; no value is ever
/// interpolated into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    Invalid,
    NotFound,
    /// Another person's or another computer's connection.
    Forbidden,
    /// The same request id with other arguments, or of another command.
    Conflict,
    RevisionConflict,
    /// Another command of this connection, or another connect of this person,
    /// has not finished.
    Busy,
    ConnectionLimit,
    ItemLimit,
    NameInUse,
    AccountInUse,
    WrongAccount,
    /// The connection is not in a state that admits the command.
    Unavailable,
    Storage,
}
impl ConnectionError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::NotFound => "not_found",
            Self::Forbidden => "wrong_owner",
            Self::Conflict => "request_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::Busy => "connection_operation_pending",
            Self::ConnectionLimit => "connection_limit",
            Self::ItemLimit => "item_limit",
            Self::NameInUse => "name_in_use",
            Self::AccountInUse => "account_in_use",
            Self::WrongAccount => "wrong_account",
            Self::Unavailable => "connection_unavailable",
            Self::Storage => "storage_error",
        }
    }
}
impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ConnectionError {}

#[cfg(test)]
mod tests;
