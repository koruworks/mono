// SPDX-License-Identifier: MPL-2.0

//! ai.db's connection rows for the connection service: each call is one
//! transaction of the writer, asked for from another thread.
//!
//! The service must not run on the writer thread (it waits on the credential
//! store and a vendor), and only the writer thread holds the repository. So a
//! call is a closure sent to the writer and a bounded wait for its answer.
//! The closure and its answer hold rows, ids and codes; the port has no place
//! for a secret or for content that was read.

use super::*;
use koru_ai_core::bots::{BotOwner, BotRequestId};
use koru_ai_core::connections::{
    ConnectedAccount, Connection, ConnectionCommandAdmission, ConnectionCommandReceipt,
    ConnectionError, ConnectionFailure, ConnectionId, ConnectionItem, ConnectionItemId,
    PickedConnectionItem, VendorRevoke,
};
use koru_connections::ConnectionStore;

/// One transaction on the writer's repository.
pub(in crate::worker) type StoreCall = Box<dyn FnOnce(&mut Repository) + Send>;

/// How long a call waits for the writer. The writer answers in milliseconds;
/// a wait this long means it is gone or stuck, and the caller is told the
/// rows are unavailable rather than held.
const STORE_WITHIN: Duration = Duration::from_secs(10);

/// Where one transaction on the connection rows runs.
pub(crate) trait Rows: Send + Sync {
    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Repository) -> Result<T, ConnectionError> + Send + 'static,
    ) -> Result<T, ConnectionError>;
}

/// The service's port over ai.db's writer, method for method the
/// repository's own functions. Production runs each on the worker's writer
/// thread ([`WriterRows`]); the one mapping is what the tests exercise too.
pub(crate) struct RowStore<R: Rows>(pub(crate) R);

/// The production store: every transaction is the writer thread's.
pub(super) type WorkerConnectionStore = RowStore<WriterRows>;
impl WorkerConnectionStore {
    pub(super) fn new(sender: SyncSender<Envelope>) -> Self {
        Self(WriterRows { sender })
    }
}

pub(crate) struct WriterRows {
    sender: SyncSender<Envelope>,
}
impl Rows for WriterRows {
    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Repository) -> Result<T, ConnectionError> + Send + 'static,
    ) -> Result<T, ConnectionError> {
        let deadline = Instant::now() + STORE_WITHIN;
        let (reply, response) = sync_channel(1);
        let mut envelope = Envelope::Connections(Box::new(move |repository: &mut Repository| {
            let _ = reply.send(work(repository));
        }));
        // A full inbox is a busy writer, not a failure: wait for room, within
        // the same bound.
        loop {
            match self.sender.try_send(envelope) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    envelope = returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return Err(ConnectionError::Storage),
            }
        }
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| ConnectionError::Storage)?
    }
}

impl<R: Rows> ConnectionStore for RowStore<R> {
    fn connection(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Option<Connection>, ConnectionError> {
        self.0.call(move |r| r.connection_by_id(owner, id))
    }
    fn live_connections(&self, owner: BotOwner) -> Result<Vec<Connection>, ConnectionError> {
        self.0.call(move |r| r.live_connections(owner))
    }
    fn connection_item(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
    ) -> Result<Option<(Connection, ConnectionItem)>, ConnectionError> {
        self.0.call(move |r| r.connection_item(owner, id))
    }
    fn receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
        self.0.call(move |r| r.connection_receipt(owner, request))
    }
    fn prepared_commands(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ConnectionCommandReceipt)>, ConnectionError> {
        self.0
            .call(move |r| r.prepared_connection_commands(after, limit))
    }

    fn begin_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        planned: ConnectionId,
        name: &str,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let (fingerprint, name) = (fingerprint.to_owned(), name.to_owned());
        self.0
            .call(move |r| r.begin_connect(owner, request, &fingerprint, planned, &name, now_ms))
    }
    fn begin_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let fingerprint = fingerprint.to_owned();
        self.0.call(move |r| {
            r.begin_sign_in_again(
                owner,
                request,
                &fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let account = account.clone();
        self.0
            .call(move |r| r.complete_connect(owner, request, &account, now_ms))
    }
    fn complete_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let account = account.clone();
        self.0
            .call(move |r| r.complete_sign_in_again(owner, request, &account, now_ms))
    }
    fn begin_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let fingerprint = fingerprint.to_owned();
        self.0.call(move |r| {
            r.begin_add_items(
                owner,
                request,
                &fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        items: &[PickedConnectionItem],
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let items = items.to_vec();
        self.0
            .call(move |r| r.complete_add_items(owner, request, &items, now_ms))
    }
    fn begin_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<(ConnectionCommandAdmission, Connection), ConnectionError> {
        let fingerprint = fingerprint.to_owned();
        self.0.call(move |r| {
            r.begin_disconnect(
                owner,
                request,
                &fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        revoke: VendorRevoke,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.0
            .call(move |r| r.complete_disconnect(owner, request, revoke, now_ms))
    }
    fn fail_command(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        failure: ConnectionFailure,
        now_ms: i64,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        self.0
            .call(move |r| r.fail_connection_command(owner, request, failure, now_ms))
    }

    fn needs_sign_in(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.0
            .call(move |r| r.connection_needs_sign_in(owner, connection, now_ms))
    }
    fn refreshed(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<(), ConnectionError> {
        self.0
            .call(move |r| r.connection_refreshed(owner, connection, now_ms))
    }
    fn item_changed(
        &self,
        owner: BotOwner,
        item: ConnectionItemId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.0
            .call(move |r| r.connection_item_changed(owner, item, now_ms))
    }
}
