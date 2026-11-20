// SPDX-License-Identifier: MPL-2.0
//! Test-only metadata at the service's storage boundary. This fixture does
//! not model SQLite or provide a runtime persistence implementation.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct MemoryStore {
    state: Mutex<State>,
    pub(crate) fail_complete: AtomicBool,
    pub(crate) panic_on_item: AtomicBool,
}

#[derive(Default)]
struct State {
    connections: BTreeMap<ConnectionId, Connection>,
    items: BTreeMap<ConnectionItemId, ConnectionItem>,
    receipts: Vec<ConnectionCommandReceipt>,
}

impl State {
    fn connection(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<&Connection, ConnectionError> {
        let connection = self.connections.get(&id).ok_or(ConnectionError::NotFound)?;
        if connection.owner != owner {
            return Err(ConnectionError::Forbidden);
        }
        Ok(connection)
    }

    fn receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<&ConnectionCommandReceipt>, ConnectionError> {
        let receipt = self
            .receipts
            .iter()
            .find(|receipt| receipt.request == request);
        if receipt.is_some_and(|receipt| receipt.owner != owner) {
            return Err(ConnectionError::Forbidden);
        }
        Ok(receipt)
    }

    fn replay(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
    ) -> Result<Option<ConnectionCommandAdmission>, ConnectionError> {
        match self.receipt(owner, request)? {
            Some(receipt) if receipt.fingerprint == fingerprint => {
                Ok(Some(ConnectionCommandAdmission::Retained(receipt.clone())))
            }
            Some(_) => Err(ConnectionError::Conflict),
            None => Ok(None),
        }
    }

    fn prepare(
        &mut self,
        receipt: ConnectionCommandReceipt,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        if self.receipts.iter().any(|prior| {
            prior.owner == receipt.owner
                && prior.stage == ConnectionReceiptStage::Prepared
                && prior.connection == receipt.connection
        }) {
            return Err(ConnectionError::Busy);
        }
        self.receipts.push(receipt.clone());
        Ok(ConnectionCommandAdmission::Prepared(receipt))
    }

    fn command(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        self.receipt(owner, request)?
            .cloned()
            .ok_or(ConnectionError::NotFound)
    }

    fn complete(&mut self, request: BotRequestId, connection: &Connection, now_ms: i64) {
        let receipt = self
            .receipts
            .iter_mut()
            .find(|receipt| receipt.request == request)
            .unwrap();
        receipt.connection = Some(connection.id);
        receipt.result_revision = Some(connection.revision);
        receipt.stage = ConnectionReceiptStage::Completed;
        receipt.updated_at_ms = now_ms;
    }

    fn pick(&mut self, connection: ConnectionId, items: &[PickedConnectionItem], now_ms: i64) {
        for picked in items {
            if let Some(item) = self.items.values_mut().find(|item| {
                item.connection == connection && item.item_ref_digest == picked.item_ref_digest
            }) {
                item.identity_digest = picked.identity_digest;
                item.title = picked.title.clone();
                item.state = ConnectionItemState::Available;
                item.changed_at_ms = None;
                item.updated_at_ms = now_ms;
            } else {
                let id = ConnectionItemId::mint();
                self.items.insert(
                    id,
                    ConnectionItem {
                        id,
                        connection,
                        item_ref: picked.item_ref.clone(),
                        item_ref_digest: picked.item_ref_digest,
                        identity_digest: picked.identity_digest,
                        title: picked.title.clone(),
                        state: ConnectionItemState::Available,
                        added_at_ms: now_ms,
                        changed_at_ms: None,
                        updated_at_ms: now_ms,
                    },
                );
            }
        }
    }
}

impl MemoryStore {
    pub(crate) fn with<T>(&self, read: impl FnOnce(&Self) -> T) -> T {
        read(self)
    }

    pub(crate) fn items(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Vec<ConnectionItem>, ConnectionError> {
        let state = self.state.lock().unwrap();
        state.connection(owner, id)?;
        Ok(state
            .items
            .values()
            .filter(|item| item.connection == id)
            .cloned()
            .collect())
    }

    fn begin_existing(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        id: ConnectionId,
        expected_revision: u64,
        kind: ConnectionCommandKind,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        if let Some(replay) = state.replay(owner, request, fingerprint)? {
            return Ok(replay);
        }
        let connection = state.connection(owner, id)?;
        if connection.revision != expected_revision {
            return Err(ConnectionError::RevisionConflict);
        }
        let available = match kind {
            ConnectionCommandKind::Connect => connection.state == ConnectionState::NeedsSignIn,
            ConnectionCommandKind::AddItems => connection.state == ConnectionState::Connected,
            ConnectionCommandKind::Disconnect => connection.state != ConnectionState::Disconnected,
        };
        if !available {
            return Err(ConnectionError::Unavailable);
        }
        let receipt = ConnectionCommandReceipt {
            request,
            owner,
            kind,
            fingerprint: fingerprint.to_owned(),
            planned_connection: (kind == ConnectionCommandKind::Connect).then_some(id),
            connection: Some(id),
            stage: ConnectionReceiptStage::Prepared,
            failure: None,
            expected_revision: Some(expected_revision),
            result_revision: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        let admission = state.prepare(receipt)?;
        if kind == ConnectionCommandKind::Disconnect {
            let connection = state.connections.get_mut(&id).unwrap();
            if connection.state != ConnectionState::Disconnecting {
                connection.state = ConnectionState::Disconnecting;
                connection.revision += 1;
                connection.updated_at_ms = now_ms;
            }
        }
        Ok(admission)
    }
}

impl ConnectionStore for MemoryStore {
    fn connection(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Option<Connection>, ConnectionError> {
        let state = self.state.lock().unwrap();
        match state.connection(owner, id) {
            Ok(connection) => Ok(Some(connection.clone())),
            Err(ConnectionError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn live_connections(&self, owner: BotOwner) -> Result<Vec<Connection>, ConnectionError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .connections
            .values()
            .filter(|connection| {
                connection.owner == owner && connection.state != ConnectionState::Disconnected
            })
            .cloned()
            .collect())
    }

    fn connection_item(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
    ) -> Result<Option<(Connection, ConnectionItem)>, ConnectionError> {
        if self.panic_on_item.load(Ordering::SeqCst) {
            panic!("the scripted store panicked");
        }
        let state = self.state.lock().unwrap();
        let Some(item) = state.items.get(&id) else {
            return Ok(None);
        };
        Ok(Some((
            state.connection(owner, item.connection)?.clone(),
            item.clone(),
        )))
    }

    fn receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
        Ok(self.state.lock().unwrap().receipt(owner, request)?.cloned())
    }

    fn prepared_commands(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ConnectionCommandReceipt)>, ConnectionError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .receipts
            .iter()
            .enumerate()
            .filter(|(index, receipt)| {
                (*index as i64 + 1) > after && receipt.stage == ConnectionReceiptStage::Prepared
            })
            .take(limit)
            .map(|(index, receipt)| (index as i64 + 1, receipt.clone()))
            .collect())
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
        let mut state = self.state.lock().unwrap();
        if let Some(replay) = state.replay(owner, request, fingerprint)? {
            return Ok(replay);
        }
        if !valid_connection_name(name) || !valid_connection_fingerprint(fingerprint) {
            return Err(ConnectionError::Invalid);
        }
        if state.connections.values().any(|connection| {
            connection.owner == owner
                && connection.state != ConnectionState::Disconnected
                && connection.name == name
        }) {
            return Err(ConnectionError::NameInUse);
        }
        state.prepare(ConnectionCommandReceipt {
            request,
            owner,
            kind: ConnectionCommandKind::Connect,
            fingerprint: fingerprint.to_owned(),
            planned_connection: Some(planned),
            connection: None,
            stage: ConnectionReceiptStage::Prepared,
            failure: None,
            expected_revision: None,
            result_revision: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
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
        self.begin_existing(
            owner,
            request,
            fingerprint,
            connection,
            expected_revision,
            ConnectionCommandKind::Connect,
            now_ms,
        )
    }

    fn complete_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        if self.fail_complete.load(Ordering::SeqCst) {
            return Err(ConnectionError::Storage);
        }
        account.validate()?;
        let mut state = self.state.lock().unwrap();
        let receipt = state.command(owner, request)?;
        let id = receipt.planned_connection.ok_or(ConnectionError::Invalid)?;
        if receipt.stage == ConnectionReceiptStage::Completed {
            return state.connection(owner, id).cloned();
        }
        if state.connections.values().any(|connection| {
            connection.owner == owner
                && connection.state != ConnectionState::Disconnected
                && connection.account_subject_digest == account.account_subject_digest
        }) {
            return Err(ConnectionError::AccountInUse);
        }
        let connection = Connection {
            id,
            owner,
            provider: account.provider,
            service_origin: account.service_origin.clone(),
            name: account.name.clone(),
            account_subject_digest: account.account_subject_digest,
            account_label: account.account_label.clone(),
            scopes: account.scopes.clone(),
            credential_slot_digest: account.credential_slot_digest,
            state: ConnectionState::Connected,
            vendor_revoke: None,
            revision: 1,
            connected_at_ms: now_ms,
            refreshed_at_ms: None,
            disconnected_at_ms: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        state.connections.insert(id, connection.clone());
        state.pick(id, &account.items, now_ms);
        state.complete(request, &connection, now_ms);
        Ok(connection)
    }

    fn complete_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        let receipt = state.command(owner, request)?;
        let id = receipt.connection.ok_or(ConnectionError::Invalid)?;
        let before = state.connection(owner, id)?;
        if receipt.stage == ConnectionReceiptStage::Completed {
            return Ok(before.clone());
        }
        if before.account_subject_digest != account.account_subject_digest {
            return Err(ConnectionError::WrongAccount);
        }
        let connection = state.connections.get_mut(&id).unwrap();
        connection.state = ConnectionState::Connected;
        connection.revision += 1;
        connection.account_label = account.account_label.clone();
        connection.scopes = account.scopes.clone();
        connection.updated_at_ms = now_ms;
        let connection = connection.clone();
        state.complete(request, &connection, now_ms);
        Ok(connection)
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
        self.begin_existing(
            owner,
            request,
            fingerprint,
            connection,
            expected_revision,
            ConnectionCommandKind::AddItems,
            now_ms,
        )
    }

    fn complete_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        items: &[PickedConnectionItem],
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        let receipt = state.command(owner, request)?;
        let id = receipt.connection.ok_or(ConnectionError::Invalid)?;
        let before = state.connection(owner, id)?;
        if receipt.stage == ConnectionReceiptStage::Completed {
            return Ok(before.clone());
        }
        state.pick(id, items, now_ms);
        let connection = state.connections.get_mut(&id).unwrap();
        connection.revision += 1;
        connection.updated_at_ms = now_ms;
        let connection = connection.clone();
        state.complete(request, &connection, now_ms);
        Ok(connection)
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
        let admission = self.begin_existing(
            owner,
            request,
            fingerprint,
            connection,
            expected_revision,
            ConnectionCommandKind::Disconnect,
            now_ms,
        )?;
        Ok((
            admission,
            self.connection(owner, connection)?
                .ok_or(ConnectionError::NotFound)?,
        ))
    }

    fn complete_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        revoke: VendorRevoke,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        let receipt = state.command(owner, request)?;
        let id = receipt.connection.ok_or(ConnectionError::Invalid)?;
        let before = state.connection(owner, id)?;
        if receipt.stage == ConnectionReceiptStage::Completed {
            return Ok(before.clone());
        }
        let connection = state.connections.get_mut(&id).unwrap();
        connection.state = ConnectionState::Disconnected;
        connection.vendor_revoke = Some(revoke);
        connection.revision += 1;
        connection.disconnected_at_ms = Some(now_ms);
        connection.updated_at_ms = now_ms;
        let connection = connection.clone();
        state.complete(request, &connection, now_ms);
        Ok(connection)
    }

    fn fail_command(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        failure: ConnectionFailure,
        now_ms: i64,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        state.command(owner, request)?;
        let receipt = state
            .receipts
            .iter_mut()
            .find(|receipt| receipt.request == request)
            .unwrap();
        if receipt.stage == ConnectionReceiptStage::Prepared {
            receipt.stage = ConnectionReceiptStage::Failed;
            receipt.failure = Some(failure);
            receipt.updated_at_ms = now_ms;
        }
        Ok(receipt.clone())
    }

    fn needs_sign_in(
        &self,
        owner: BotOwner,
        id: ConnectionId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        state.connection(owner, id)?;
        let connection = state.connections.get_mut(&id).unwrap();
        if connection.state == ConnectionState::Connected {
            connection.state = ConnectionState::NeedsSignIn;
            connection.revision += 1;
            connection.updated_at_ms = now_ms;
        }
        Ok(connection.clone())
    }

    fn refreshed(
        &self,
        owner: BotOwner,
        id: ConnectionId,
        now_ms: i64,
    ) -> Result<(), ConnectionError> {
        let mut state = self.state.lock().unwrap();
        state.connection(owner, id)?;
        let connection = state.connections.get_mut(&id).unwrap();
        connection.refreshed_at_ms = Some(now_ms);
        connection.updated_at_ms = now_ms;
        Ok(())
    }

    fn item_changed(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        let mut state = self.state.lock().unwrap();
        let item = state.items.get(&id).ok_or(ConnectionError::NotFound)?;
        let connection_id = item.connection;
        state.connection(owner, connection_id)?;
        let item = state.items.get_mut(&id).unwrap();
        if item.state == ConnectionItemState::Available {
            item.state = ConnectionItemState::Changed;
            item.changed_at_ms = Some(now_ms);
            item.updated_at_ms = now_ms;
            let connection = state.connections.get_mut(&connection_id).unwrap();
            connection.revision += 1;
            connection.updated_at_ms = now_ms;
        }
        Ok(state.connections[&connection_id].clone())
    }
}
