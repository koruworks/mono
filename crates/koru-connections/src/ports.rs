// SPDX-License-Identifier: MPL-2.0

//! What the service is given: ai.db's connection rows, the delegate's effect
//! ledger, a clock, and a place to say that a connection changed. None of
//! them can carry a secret or content: the signatures have no place for one.

use std::time::{Duration, Instant};

use koru_ai_core::bots::{BotOwner, BotRequestId};
use koru_ai_core::connections::{
    ConnectedAccount, Connection, ConnectionCommandAdmission, ConnectionCommandReceipt,
    ConnectionError, ConnectionFailure, ConnectionId, ConnectionItem, ConnectionItemId,
    PickedConnectionItem, ProviderToken, VendorRevoke,
};
use koru_delegate_core::{
    PrivateDispatcherId, PrivateEffectKey, PrivateEffectTransition, PrivateEffectView,
};

/// The connection rows of ai.db, as their one writer exposes them. Each call
/// is one transaction of the AI owner; the production implementation reaches
/// it through the AI worker, off the caller's thread. Method for method this
/// is the repository of `koru-ai-sqlite`.
pub trait ConnectionStore: Send + Sync {
    fn connection(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Option<Connection>, ConnectionError>;
    fn live_connections(&self, owner: BotOwner) -> Result<Vec<Connection>, ConnectionError>;
    fn connection_item(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
    ) -> Result<Option<(Connection, ConnectionItem)>, ConnectionError>;
    fn receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<ConnectionCommandReceipt>, ConnectionError>;
    /// One page of commands still `prepared`, of every owner, after row id
    /// `after`.
    fn prepared_commands(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ConnectionCommandReceipt)>, ConnectionError>;

    #[allow(clippy::too_many_arguments)]
    fn begin_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        planned: ConnectionId,
        name: &str,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError>;
    #[allow(clippy::too_many_arguments)]
    fn begin_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError>;
    fn complete_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
    fn complete_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
    #[allow(clippy::too_many_arguments)]
    fn begin_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError>;
    fn complete_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        items: &[PickedConnectionItem],
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
    #[allow(clippy::too_many_arguments)]
    fn begin_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<(ConnectionCommandAdmission, Connection), ConnectionError>;
    fn complete_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        revoke: VendorRevoke,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
    fn fail_command(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        failure: ConnectionFailure,
        now_ms: i64,
    ) -> Result<ConnectionCommandReceipt, ConnectionError>;

    fn needs_sign_in(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
    fn refreshed(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<(), ConnectionError>;
    fn item_changed(
        &self,
        owner: BotOwner,
        item: ConnectionItemId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError>;
}

/// Why the delegate did not record a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerError {
    /// `bot_private_effect_stale`: the row was closed by someone else, or the
    /// transition is not legal from where it stands. Whatever the dispatcher
    /// holds is dropped.
    Stale,
    /// The delegate did not answer. The command may have committed; the same
    /// advance is sent again.
    Unavailable,
    /// Any other refusal. Nothing was committed.
    Refused,
}

/// The one delegate command the dispatcher sends while it performs a read:
/// `AdvancePrivateEffect`. Idempotent by key and transition. The production
/// implementation is the private Bot channel to the delegate.
pub trait EffectLedger: Send + Sync {
    fn advance(
        &self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateEffectView, LedgerError>;
}

/// Time, so that no test waits. `sleep` is the dispatcher's back-off.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    /// Milliseconds since the Unix epoch, for a stored row.
    fn wall_ms(&self) -> i64;
    fn sleep(&self, duration: Duration);
}

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0)
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Whether whoever asked for a read is still there to receive it. Content is
/// handed over only to a caller that is.
pub trait ReadCaller {
    fn gone(&self) -> bool;
}
/// A caller that is always there: a synchronous call that cannot be dropped.
#[derive(Clone, Copy, Debug, Default)]
pub struct PresentCaller;
impl ReadCaller for PresentCaller {
    fn gone(&self) -> bool {
        false
    }
}

/// What happened to a connection, in the closed tokens of the contract's
/// `connection.changed` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionChange {
    Connected,
    NeedsSignIn,
    SignedIn,
    ItemsChanged,
    Disconnecting,
    Disconnected,
}
impl ConnectionChange {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::NeedsSignIn => "needs_sign_in",
            Self::SignedIn => "signed_in",
            Self::ItemsChanged => "items_changed",
            Self::Disconnecting => "disconnecting",
            Self::Disconnected => "disconnected",
        }
    }
}

/// One committed change to a connection: ids, closed tokens and a revision.
/// It has no field for a name, a label, a title or a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionChanged {
    pub connection: ConnectionId,
    pub provider: ProviderToken,
    pub change: ConnectionChange,
    pub revision: u64,
    /// Set only on `Disconnected`.
    pub vendor_revoke: Option<VendorRevoke>,
}

/// **The seam for the `connection.changed` event.** The event is not
/// registered: the event-name grammar has no `connection` area, and it grows
/// by review. So the service publishes nothing. It tells this sink after each
/// durable change, and the default sink drops the notice. Whoever registers
/// the event implements this trait over the event bus; nothing else in this
/// crate changes.
pub trait ConnectionChangeSink: Send + Sync {
    fn connection_changed(&self, change: ConnectionChanged);
}
/// Drops every notice. What the service uses until the event exists.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoConnectionChangeSink;
impl ConnectionChangeSink for NoConnectionChangeSink {
    fn connection_changed(&self, _: ConnectionChanged) {}
}
