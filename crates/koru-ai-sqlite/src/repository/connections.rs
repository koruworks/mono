// SPDX-License-Identifier: MPL-2.0
//! The one owner of `connections`, `connection_items`, `bot_connection_grants`
//! and `connection_command_receipts` (ai.db 25, M5).
//!
//! No secret has a column or a parameter here. A connect stores its
//! credential in the OS credential store before it calls
//! [`Repository::complete_connect`]; this module sees the digest of the
//! slot's name and nothing else. No vendor, keychain or network effect
//! happens here: each function is one transaction, and the connection
//! service performs the effect between `begin` and `complete`.
//!
//! Every write is idempotent by `(user_scope, request)`: the same request
//! with the same fingerprint returns what was retained, with another
//! fingerprint it is a conflict. `connections`, `connection_items` and the
//! receipts are never deleted; a grant row is the grant, and removing access
//! deletes it.

use std::collections::BTreeSet;

use koru_ai_core::bots::{BotError, BotId, BotOwner, BotRequestId, DeviceId, UserScopeId};
use koru_ai_core::connections::*;
use rusqlite::{OptionalExtension, Row, params};

use super::Repository;

fn bad(_: impl std::fmt::Display) -> ConnectionError {
    ConnectionError::Storage
}

const CONNECTION: &str = "connection_id,user_scope_id,owner_device_id,provider,service_origin,name,account_subject_digest,account_label,scopes_json,credential_slot_digest,state,vendor_revoke,revision,connected_at_ms,refreshed_at_ms,disconnected_at_ms,created_at_ms,updated_at_ms";
const ITEM: &str = "item_id,connection_id,item_ref,item_ref_digest,identity_digest,title,state,added_at_ms,changed_at_ms,updated_at_ms";
const RECEIPT: &str = "request_id,user_scope_id,owner_device_id,command_kind,fingerprint,planned_connection_id,connection_id,stage,failure,expected_revision,result_revision,created_at_ms,updated_at_ms";

fn digest(value: Vec<u8>) -> Result<ConnectionDigest, ConnectionError> {
    value.try_into().map_err(|_| ConnectionError::Storage)
}

struct RawConnection(
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<u8>,
    String,
    String,
    Vec<u8>,
    String,
    Option<String>,
    u64,
    i64,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
);
fn raw_connection(r: &Row<'_>) -> rusqlite::Result<RawConnection> {
    Ok(RawConnection(
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
        r.get(14)?,
        r.get(15)?,
        r.get(16)?,
        r.get(17)?,
    ))
}
impl RawConnection {
    fn decode(self) -> Result<Connection, ConnectionError> {
        Ok(Connection {
            id: self.0.parse().map_err(bad)?,
            owner: BotOwner {
                user_scope: UserScopeId::parse(&self.1).ok_or(ConnectionError::Storage)?,
                device: DeviceId::parse(&self.2).ok_or(ConnectionError::Storage)?,
            },
            provider: ProviderToken::parse(&self.3).ok_or(ConnectionError::Storage)?,
            service_origin: self.4,
            name: self.5,
            account_subject_digest: digest(self.6)?,
            account_label: self.7,
            scopes: serde_json::from_str(&self.8).map_err(bad)?,
            credential_slot_digest: digest(self.9)?,
            state: ConnectionState::parse(&self.10).ok_or(ConnectionError::Storage)?,
            vendor_revoke: self
                .11
                .map(|v| VendorRevoke::parse(&v).ok_or(ConnectionError::Storage))
                .transpose()?,
            revision: self.12,
            connected_at_ms: self.13,
            refreshed_at_ms: self.14,
            disconnected_at_ms: self.15,
            created_at_ms: self.16,
            updated_at_ms: self.17,
        })
    }
}

type RawItem = (
    String,
    String,
    String,
    Vec<u8>,
    Vec<u8>,
    String,
    String,
    i64,
    Option<i64>,
    i64,
);
fn raw_item(r: &Row<'_>) -> rusqlite::Result<RawItem> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}
fn item(raw: RawItem) -> Result<ConnectionItem, ConnectionError> {
    Ok(ConnectionItem {
        id: raw.0.parse().map_err(bad)?,
        connection: raw.1.parse().map_err(bad)?,
        item_ref: raw.2,
        item_ref_digest: digest(raw.3)?,
        identity_digest: digest(raw.4)?,
        title: raw.5,
        state: ConnectionItemState::parse(&raw.6).ok_or(ConnectionError::Storage)?,
        added_at_ms: raw.7,
        changed_at_ms: raw.8,
        updated_at_ms: raw.9,
    })
}

type RawReceipt = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<u64>,
    Option<u64>,
    i64,
    i64,
);
fn raw_receipt(r: &Row<'_>) -> rusqlite::Result<RawReceipt> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
    ))
}
fn receipt(raw: RawReceipt) -> Result<ConnectionCommandReceipt, ConnectionError> {
    let id = |value: Option<String>| -> Result<Option<ConnectionId>, ConnectionError> {
        value.map(|v| v.parse().map_err(bad)).transpose()
    };
    let receipt = ConnectionCommandReceipt {
        request: raw.0.parse().map_err(bad)?,
        owner: BotOwner {
            user_scope: UserScopeId::parse(&raw.1).ok_or(ConnectionError::Storage)?,
            device: DeviceId::parse(&raw.2).ok_or(ConnectionError::Storage)?,
        },
        kind: ConnectionCommandKind::parse(&raw.3).ok_or(ConnectionError::Storage)?,
        fingerprint: raw.4,
        planned_connection: id(raw.5)?,
        connection: id(raw.6)?,
        stage: ConnectionReceiptStage::parse(&raw.7).ok_or(ConnectionError::Storage)?,
        failure: raw
            .8
            .map(|v| ConnectionFailure::parse(&v).ok_or(ConnectionError::Storage))
            .transpose()?,
        expected_revision: raw.9,
        result_revision: raw.10,
        created_at_ms: raw.11,
        updated_at_ms: raw.12,
    };
    if !valid_connection_fingerprint(&receipt.fingerprint) {
        return Err(ConnectionError::Storage);
    }
    Ok(receipt)
}

fn connection_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    id: ConnectionId,
) -> Result<Option<Connection>, ConnectionError> {
    let raw = conn
        .query_row(
            &format!("SELECT {CONNECTION} FROM connections WHERE connection_id=?1"),
            [id.to_storage()],
            raw_connection,
        )
        .optional()
        .map_err(bad)?;
    let Some(raw) = raw else { return Ok(None) };
    let connection = raw.decode()?;
    if connection.owner != owner {
        return Err(ConnectionError::Forbidden);
    }
    Ok(Some(connection))
}

fn receipt_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    request: BotRequestId,
) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
    let raw = conn
        .query_row(
            &format!(
                "SELECT {RECEIPT} FROM connection_command_receipts WHERE user_scope_id=?1 AND request_id=?2"
            ),
            params![owner.user_scope.to_storage(), request.to_storage()],
            raw_receipt,
        )
        .optional()
        .map_err(bad)?;
    let Some(raw) = raw else { return Ok(None) };
    let receipt = receipt(raw)?;
    if receipt.owner != owner {
        return Err(ConnectionError::Forbidden);
    }
    Ok(Some(receipt))
}

/// Replay first: a retained receipt of the same kind and fingerprint answers;
/// anything else under the same request id is a conflict.
fn retained_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    request: BotRequestId,
    kind: ConnectionCommandKind,
    fingerprint: &str,
) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
    if !valid_connection_fingerprint(fingerprint) || !v4(request.as_uuid()) {
        return Err(ConnectionError::Invalid);
    }
    match receipt_on(conn, owner, request)? {
        None => Ok(None),
        Some(receipt) if receipt.kind == kind && receipt.fingerprint == fingerprint => {
            Ok(Some(receipt))
        }
        Some(_) => Err(ConnectionError::Conflict),
    }
}

fn v4(id: uuid::Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

/// The receipt named by `request`, which must be of `kind`, in whatever
/// stage it has. Each `complete` decides what a finished one answers.
fn open_receipt_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    request: BotRequestId,
    kind: ConnectionCommandKind,
) -> Result<ConnectionCommandReceipt, ConnectionError> {
    let receipt = receipt_on(conn, owner, request)?.ok_or(ConnectionError::NotFound)?;
    if receipt.kind != kind {
        return Err(ConnectionError::Conflict);
    }
    Ok(receipt)
}

fn live_count_on(conn: &rusqlite::Connection, owner: BotOwner) -> Result<usize, ConnectionError> {
    conn.query_row(
        "SELECT count(*) FROM connections WHERE user_scope_id=?1 AND owner_device_id=?2 AND state<>'disconnected'",
        params![owner.user_scope.to_storage(), owner.device.to_storage()],
        |r| r.get::<_, i64>(0),
    )
    .map(|count| count as usize)
    .map_err(bad)
}

fn name_in_use_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    name: &str,
) -> Result<bool, ConnectionError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM connections WHERE user_scope_id=?1 AND owner_device_id=?2 AND name=?3 AND state<>'disconnected')",
        params![owner.user_scope.to_storage(), owner.device.to_storage(), name],
        |r| r.get(0),
    )
    .map_err(bad)
}

fn insert_receipt(
    conn: &rusqlite::Connection,
    receipt: &ConnectionCommandReceipt,
) -> Result<(), ConnectionError> {
    conn.execute(
        "INSERT INTO connection_command_receipts(user_scope_id,request_id,owner_device_id,command_kind,fingerprint,planned_connection_id,connection_id,stage,expected_revision,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,'prepared',?8,?9,?9)",
        params![
            receipt.owner.user_scope.to_storage(),
            receipt.request.to_storage(),
            receipt.owner.device.to_storage(),
            receipt.kind.token(),
            receipt.fingerprint,
            receipt.planned_connection.map(|id| id.to_storage()),
            receipt.connection.map(|id| id.to_storage()),
            receipt.expected_revision,
            receipt.created_at_ms
        ],
    )
    .map_err(bad)?;
    Ok(())
}

fn finish_receipt(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    request: BotRequestId,
    connection: Option<ConnectionId>,
    outcome: Result<u64, ConnectionFailure>,
    now: i64,
) -> Result<(), ConnectionError> {
    let (stage, failure, revision) = match outcome {
        Ok(revision) => ("completed", None, Some(revision)),
        Err(failure) => ("failed", Some(failure.token()), None),
    };
    let changed = conn
        .execute(
            "UPDATE connection_command_receipts SET stage=?3,failure=?4,result_revision=?5,
               connection_id=coalesce(connection_id,?6),updated_at_ms=MAX(updated_at_ms,?7)
             WHERE user_scope_id=?1 AND request_id=?2 AND stage='prepared'",
            params![
                owner.user_scope.to_storage(),
                request.to_storage(),
                stage,
                failure,
                revision,
                connection.map(|id| id.to_storage()),
                now
            ],
        )
        .map_err(bad)?;
    if changed != 1 {
        return Err(ConnectionError::Conflict);
    }
    Ok(())
}

/// Insert the picked items of `connection`, or return a picked-again item to
/// `available` under its new identity. Returns whether anything changed.
fn store_items(
    conn: &rusqlite::Connection,
    connection: ConnectionId,
    items: &[PickedConnectionItem],
    now: i64,
) -> Result<bool, ConnectionError> {
    if !valid_picked_items(items) {
        return Err(ConnectionError::Invalid);
    }
    let mut changed = false;
    for picked in items {
        let existing: Option<(String, Vec<u8>, String, String)> = conn
            .query_row(
                "SELECT item_id,identity_digest,state,title FROM connection_items WHERE connection_id=?1 AND item_ref_digest=?2",
                params![connection.to_storage(), picked.item_ref_digest.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(bad)?;
        match existing {
            Some((id, identity, state, title)) => {
                if identity == picked.identity_digest
                    && state == ConnectionItemState::Available.token()
                    && title == picked.title
                {
                    continue;
                }
                // Picking an item again is the fresh grant (D17): its new
                // identity replaces the old one and it can be read again.
                conn.execute(
                    "UPDATE connection_items SET identity_digest=?2,title=?3,state='available',changed_at_ms=NULL,updated_at_ms=MAX(updated_at_ms,?4) WHERE item_id=?1",
                    params![id, picked.identity_digest.as_slice(), picked.title, now],
                )
                .map_err(bad)?;
            }
            None => {
                conn.execute(
                    "INSERT INTO connection_items(item_id,connection_id,item_ref,item_ref_digest,identity_digest,title,state,added_at_ms,updated_at_ms)
                     VALUES(?1,?2,?3,?4,?5,?6,'available',?7,?7)",
                    params![
                        ConnectionItemId::mint().to_storage(),
                        connection.to_storage(),
                        picked.item_ref,
                        picked.item_ref_digest.as_slice(),
                        picked.identity_digest.as_slice(),
                        picked.title,
                        now
                    ],
                )
                .map_err(bad)?;
            }
        }
        changed = true;
    }
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM connection_items WHERE connection_id=?1",
            [connection.to_storage()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if count as usize > CONNECTION_ITEM_LIMIT {
        return Err(ConnectionError::ItemLimit);
    }
    Ok(changed)
}

/// A Bot's grants, bounded: one more than the limit is a damaged table.
pub(crate) fn bot_grants_on(
    conn: &rusqlite::Connection,
    bot: BotId,
) -> Result<BTreeSet<ConnectionGrantRef>, BotError> {
    let mut query = conn
        .prepare_cached(
            "SELECT CASE WHEN typeof(connection_id)='text' AND length(CAST(connection_id AS BLOB))=36 THEN connection_id END,
                    CASE WHEN typeof(operation)='text' AND length(CAST(operation AS BLOB))<=16 THEN operation END
             FROM bot_connection_grants WHERE bot_id=?1 ORDER BY connection_id,operation LIMIT 17",
        )
        .map_err(|_| BotError::Storage)?;
    let rows = query
        .query_map([bot.to_storage()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| BotError::Storage)?;
    let mut grants = BTreeSet::new();
    for row in rows {
        let (connection, operation) = row.map_err(|_| BotError::Storage)?;
        if operation != "read_item" {
            return Err(BotError::Storage);
        }
        grants.insert(ConnectionGrantRef {
            connection: connection.parse().map_err(|_| BotError::Storage)?,
            operation: PrivateOperationKind::ReadItem,
        });
    }
    if grants.len() > CONNECTION_GRANT_LIMIT {
        return Err(BotError::Storage);
    }
    Ok(grants)
}

/// Make the Bot's grant rows equal `desired`, inside the Bot writer's
/// transaction. A grant that stays keeps its `granted_at_ms`. A grant that is
/// new is accepted only for a connection of the same person on this computer
/// that is `connected`; a grant the Bot already holds may point at a
/// connection in any state, a tombstone included, and authorizes nothing by
/// itself.
pub(crate) fn replace_bot_grants_on(
    conn: &rusqlite::Connection,
    owner: BotOwner,
    bot: BotId,
    desired: &BTreeSet<ConnectionGrantRef>,
    now: i64,
) -> Result<(), BotError> {
    if desired.len() > CONNECTION_GRANT_LIMIT {
        return Err(BotError::Invalid);
    }
    let current = bot_grants_on(conn, bot)?;
    if &current == desired {
        return Ok(());
    }
    for removed in current.difference(desired) {
        conn.execute(
            "DELETE FROM bot_connection_grants WHERE bot_id=?1 AND connection_id=?2 AND operation='read_item'",
            params![bot.to_storage(), removed.connection.to_storage()],
        )
        .map_err(|_| BotError::Storage)?;
    }
    for added in desired.difference(&current) {
        let usable: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM connections WHERE connection_id=?1 AND user_scope_id=?2 AND owner_device_id=?3 AND state='connected')",
                params![
                    added.connection.to_storage(),
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |r| r.get(0),
            )
            .map_err(|_| BotError::Storage)?;
        if !usable {
            return Err(BotError::ConnectionUnavailable);
        }
        conn.execute(
            "INSERT INTO bot_connection_grants(bot_id,connection_id,operation,granted_at_ms) VALUES(?1,?2,'read_item',?3)",
            params![bot.to_storage(), added.connection.to_storage(), now],
        )
        .map_err(|_| BotError::Storage)?;
    }
    Ok(())
}

impl Repository {
    /// One connection in any state, a tombstone included. Another person's or
    /// another computer's is `Forbidden`.
    pub fn connection_by_id(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Option<Connection>, ConnectionError> {
        connection_on(&self.connection, owner, id)
    }

    /// The owner's connections that are not disconnected, oldest first. At
    /// most [`CONNECTION_LIMIT`] exist.
    pub fn live_connections(&self, owner: BotOwner) -> Result<Vec<Connection>, ConnectionError> {
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT {CONNECTION} FROM connections WHERE user_scope_id=?1 AND owner_device_id=?2 AND state<>'disconnected' ORDER BY created_at_ms,connection_id LIMIT 64"
            ))
            .map_err(bad)?;
        let rows = query
            .query_map(
                params![owner.user_scope.to_storage(), owner.device.to_storage()],
                raw_connection,
            )
            .map_err(bad)?;
        rows.map(|row| row.map_err(bad)?.decode()).collect()
    }

    /// Every item of one connection, in the order they were added.
    pub fn connection_items(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
    ) -> Result<Vec<ConnectionItem>, ConnectionError> {
        connection_on(&self.connection, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT {ITEM} FROM connection_items WHERE connection_id=?1 ORDER BY added_at_ms,item_id LIMIT 257"
            ))
            .map_err(bad)?;
        let rows = query
            .query_map([connection.to_storage()], raw_item)
            .map_err(bad)?;
        rows.map(|row| item(row.map_err(bad)?)).collect()
    }

    /// One item with its connection, under the owner fence.
    pub fn connection_item(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
    ) -> Result<Option<(Connection, ConnectionItem)>, ConnectionError> {
        let raw = self
            .connection
            .query_row(
                &format!("SELECT {ITEM} FROM connection_items WHERE item_id=?1"),
                [id.to_storage()],
                raw_item,
            )
            .optional()
            .map_err(bad)?;
        let Some(raw) = raw else { return Ok(None) };
        let item = item(raw)?;
        let connection = connection_on(&self.connection, owner, item.connection)?
            .ok_or(ConnectionError::Storage)?;
        Ok(Some((connection, item)))
    }

    /// The owner's items with this vendor reference, on connections that are
    /// not disconnected: how a link a person gave is found among what they
    /// picked. More than one result means two accounts hold the item.
    pub fn connection_items_by_ref(
        &self,
        owner: BotOwner,
        provider: ProviderToken,
        item_ref_digest: &ConnectionDigest,
    ) -> Result<Vec<(Connection, ConnectionItem)>, ConnectionError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT i.item_id FROM connection_items i JOIN connections c ON c.connection_id=i.connection_id
                 WHERE c.user_scope_id=?1 AND c.owner_device_id=?2 AND c.provider=?3 AND c.state<>'disconnected' AND i.item_ref_digest=?4
                 ORDER BY c.created_at_ms,c.connection_id LIMIT 9",
            )
            .map_err(bad)?;
        let ids = query
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    provider.as_str(),
                    item_ref_digest.as_slice()
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(bad)?;
        let mut found = Vec::new();
        for id in ids {
            let id: ConnectionItemId = id.map_err(bad)?.parse().map_err(bad)?;
            found.push(
                self.connection_item(owner, id)?
                    .ok_or(ConnectionError::Storage)?,
            );
        }
        Ok(found)
    }

    /// One retained connection command, by its request.
    pub fn connection_receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
        receipt_on(&self.connection, owner, request)
    }

    /// One page of commands still `prepared`, of every owner, for the
    /// service's start: an interrupted connect is failed and its planned slot
    /// cleared; a disconnect is finished. `after` is the previous page's last
    /// row id.
    pub fn prepared_connection_commands(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ConnectionCommandReceipt)>, ConnectionError> {
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT rowid,{RECEIPT} FROM connection_command_receipts WHERE rowid>?1 AND stage='prepared' ORDER BY rowid LIMIT ?2"
            ))
            .map_err(bad)?;
        let rows = query
            .query_map(params![after, limit.clamp(1, 64) as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                        r.get(12)?,
                        r.get(13)?,
                    ),
                ))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (rowid, raw) = row.map_err(bad)?;
            Ok((rowid, receipt(raw)?))
        })
        .collect()
    }

    /// Begin a connect: mint nothing, record `planned` and return. One
    /// connect (or sign-in again) at a time per person. Refused before any
    /// receipt is written when the person already holds
    /// [`CONNECTION_LIMIT`] live connections or uses `name` for one.
    pub fn begin_connect(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        planned: ConnectionId,
        name: &str,
        now: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        if let Some(retained) = retained_on(
            &tx,
            owner,
            request,
            ConnectionCommandKind::Connect,
            fingerprint,
        )? {
            return Ok(ConnectionCommandAdmission::Retained(retained));
        }
        if !valid_connection_name(name) || !v4(planned.as_uuid()) {
            return Err(ConnectionError::Invalid);
        }
        let taken: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM connections WHERE connection_id=?1)
                     OR EXISTS(SELECT 1 FROM connection_command_receipts WHERE planned_connection_id=?1)",
                [planned.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if taken {
            return Err(ConnectionError::Conflict);
        }
        connect_open_on(&tx, owner)?;
        if live_count_on(&tx, owner)? >= CONNECTION_LIMIT {
            return Err(ConnectionError::ConnectionLimit);
        }
        if name_in_use_on(&tx, owner, name)? {
            return Err(ConnectionError::NameInUse);
        }
        let receipt = ConnectionCommandReceipt {
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
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_receipt(&tx, &receipt)?;
        tx.commit().map_err(bad)?;
        Ok(ConnectionCommandAdmission::Prepared(receipt))
    }

    /// Begin signing in again to a connection in `needs_sign_in`. It is a
    /// `connect` receipt whose planned connection is the existing one, so the
    /// credential slot is the connection's own.
    pub fn begin_sign_in_again(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        if let Some(retained) = retained_on(
            &tx,
            owner,
            request,
            ConnectionCommandKind::Connect,
            fingerprint,
        )? {
            return Ok(ConnectionCommandAdmission::Retained(retained));
        }
        let current = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        if current.state != ConnectionState::NeedsSignIn {
            return Err(ConnectionError::Unavailable);
        }
        if current.revision != expected_revision {
            return Err(ConnectionError::RevisionConflict);
        }
        connect_open_on(&tx, owner)?;
        command_open_on(&tx, connection)?;
        let receipt = ConnectionCommandReceipt {
            request,
            owner,
            kind: ConnectionCommandKind::Connect,
            fingerprint: fingerprint.to_owned(),
            planned_connection: Some(connection),
            connection: Some(connection),
            stage: ConnectionReceiptStage::Prepared,
            failure: None,
            expected_revision: Some(expected_revision),
            result_revision: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_receipt(&tx, &receipt)?;
        tx.commit().map_err(bad)?;
        Ok(ConnectionCommandAdmission::Prepared(receipt))
    }

    /// Complete a connect: one transaction inserts the connection under the
    /// planned id with its picked items and completes the receipt. The
    /// caller stored the credential in the planned slot first, and clears it
    /// if this fails. `AccountInUse` and `NameInUse` write nothing; the
    /// caller then fails the receipt with that reason. Repeating a completed
    /// connect returns its connection.
    pub fn complete_connect(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        account.validate()?;
        let tx = self.connection.transaction().map_err(bad)?;
        let receipt = open_receipt_on(&tx, owner, request, ConnectionCommandKind::Connect)?;
        let planned = receipt.planned_connection.ok_or(ConnectionError::Storage)?;
        if receipt.connection.is_some() && receipt.stage == ConnectionReceiptStage::Prepared {
            // A sign-in again is completed by its own function.
            return Err(ConnectionError::Conflict);
        }
        match receipt.stage {
            ConnectionReceiptStage::Completed => {
                return connection_on(&tx, owner, planned)?.ok_or(ConnectionError::Storage);
            }
            ConnectionReceiptStage::Failed => return Err(ConnectionError::Conflict),
            ConnectionReceiptStage::Prepared => {}
        }
        let in_use: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM connections WHERE user_scope_id=?1 AND owner_device_id=?2 AND provider=?3 AND account_subject_digest=?4 AND state<>'disconnected')",
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    account.provider.as_str(),
                    account.account_subject_digest.as_slice()
                ],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if in_use {
            return Err(ConnectionError::AccountInUse);
        }
        if name_in_use_on(&tx, owner, &account.name)? {
            return Err(ConnectionError::NameInUse);
        }
        if live_count_on(&tx, owner)? >= CONNECTION_LIMIT {
            return Err(ConnectionError::ConnectionLimit);
        }
        tx.execute(
            "INSERT INTO connections(connection_id,user_scope_id,owner_device_id,provider,service_origin,name,account_subject_digest,account_label,scopes_json,credential_slot_digest,state,revision,connected_at_ms,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'connected',1,?11,?11,?11)",
            params![
                planned.to_storage(),
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                account.provider.as_str(),
                account.service_origin,
                account.name,
                account.account_subject_digest.as_slice(),
                account.account_label,
                scopes_json(&account.scopes).ok_or(ConnectionError::Invalid)?,
                account.credential_slot_digest.as_slice(),
                now
            ],
        )
        .map_err(bad)?;
        store_items(&tx, planned, &account.items, now)?;
        finish_receipt(&tx, owner, request, Some(planned), Ok(1), now)?;
        let connection = connection_on(&tx, owner, planned)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }

    /// Complete a sign-in again. The account must be the connection's own:
    /// another subject digest is `WrongAccount` and writes nothing. The
    /// caller compares the account before it stores the token, so this is
    /// the second check, not the first. The label and scopes may be
    /// refreshed; picked items are added.
    pub fn complete_sign_in_again(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        account.validate()?;
        let tx = self.connection.transaction().map_err(bad)?;
        let receipt = open_receipt_on(&tx, owner, request, ConnectionCommandKind::Connect)?;
        let id = receipt.connection.ok_or(ConnectionError::Conflict)?;
        let current = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        match receipt.stage {
            ConnectionReceiptStage::Completed => return Ok(current),
            ConnectionReceiptStage::Failed => return Err(ConnectionError::Conflict),
            ConnectionReceiptStage::Prepared => {}
        }
        if current.provider != account.provider
            || current.account_subject_digest != account.account_subject_digest
        {
            return Err(ConnectionError::WrongAccount);
        }
        if current.credential_slot_digest != account.credential_slot_digest
            || current.service_origin != account.service_origin
        {
            return Err(ConnectionError::Invalid);
        }
        if current.state != ConnectionState::NeedsSignIn
            || Some(current.revision) != receipt.expected_revision
        {
            return Err(ConnectionError::RevisionConflict);
        }
        let revision = current.revision + 1;
        tx.execute(
            "UPDATE connections SET state='connected',revision=?2,account_label=?3,scopes_json=?4,refreshed_at_ms=?5,updated_at_ms=MAX(updated_at_ms,?5) WHERE connection_id=?1",
            params![
                id.to_storage(),
                revision,
                account.account_label,
                scopes_json(&account.scopes).ok_or(ConnectionError::Invalid)?,
                now
            ],
        )
        .map_err(bad)?;
        store_items(&tx, id, &account.items, now)?;
        finish_receipt(&tx, owner, request, None, Ok(revision), now)?;
        let connection = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }

    /// Begin the picker trip again for a connected connection.
    pub fn begin_add_items(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        if let Some(retained) = retained_on(
            &tx,
            owner,
            request,
            ConnectionCommandKind::AddItems,
            fingerprint,
        )? {
            return Ok(ConnectionCommandAdmission::Retained(retained));
        }
        let current = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        if current.state != ConnectionState::Connected {
            return Err(ConnectionError::Unavailable);
        }
        if current.revision != expected_revision {
            return Err(ConnectionError::RevisionConflict);
        }
        command_open_on(&tx, connection)?;
        let receipt = ConnectionCommandReceipt {
            request,
            owner,
            kind: ConnectionCommandKind::AddItems,
            fingerprint: fingerprint.to_owned(),
            planned_connection: None,
            connection: Some(connection),
            stage: ConnectionReceiptStage::Prepared,
            failure: None,
            expected_revision: Some(expected_revision),
            result_revision: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_receipt(&tx, &receipt)?;
        tx.commit().map_err(bad)?;
        Ok(ConnectionCommandAdmission::Prepared(receipt))
    }

    /// Add what the person picked. An item picked again returns to
    /// `available` under its new identity. The revision rises by one.
    /// `ItemLimit` writes nothing; the caller fails the receipt with it.
    pub fn complete_add_items(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        items: &[PickedConnectionItem],
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let receipt = open_receipt_on(&tx, owner, request, ConnectionCommandKind::AddItems)?;
        let id = receipt.connection.ok_or(ConnectionError::Storage)?;
        let current = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        match receipt.stage {
            ConnectionReceiptStage::Completed => return Ok(current),
            ConnectionReceiptStage::Failed => return Err(ConnectionError::Conflict),
            ConnectionReceiptStage::Prepared => {}
        }
        if current.state != ConnectionState::Connected
            || Some(current.revision) != receipt.expected_revision
        {
            return Err(ConnectionError::RevisionConflict);
        }
        store_items(&tx, id, items, now)?;
        let revision = current.revision + 1;
        tx.execute(
            "UPDATE connections SET revision=?2,updated_at_ms=MAX(updated_at_ms,?3) WHERE connection_id=?1",
            params![id.to_storage(), revision, now],
        )
        .map_err(bad)?;
        finish_receipt(&tx, owner, request, None, Ok(revision), now)?;
        let connection = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }

    /// Step one of Disconnect: one transaction writes the `prepared` receipt,
    /// sets the connection to `disconnecting` and raises its revision. From
    /// this commit no read is dispatched. A connection already
    /// `disconnecting` (an earlier Disconnect could not clear the credential
    /// store) accepts a new request and is not changed again.
    pub fn begin_disconnect(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now: i64,
    ) -> Result<(ConnectionCommandAdmission, Connection), ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        if let Some(retained) = retained_on(
            &tx,
            owner,
            request,
            ConnectionCommandKind::Disconnect,
            fingerprint,
        )? {
            let id = retained.connection.ok_or(ConnectionError::Storage)?;
            let current = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
            return Ok((ConnectionCommandAdmission::Retained(retained), current));
        }
        let current = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        if current.state == ConnectionState::Disconnected {
            return Err(ConnectionError::Unavailable);
        }
        if current.revision != expected_revision {
            return Err(ConnectionError::RevisionConflict);
        }
        command_open_on(&tx, connection)?;
        if current.state != ConnectionState::Disconnecting {
            tx.execute(
                "UPDATE connections SET state='disconnecting',revision=revision+1,updated_at_ms=MAX(updated_at_ms,?2) WHERE connection_id=?1",
                params![connection.to_storage(), now],
            )
            .map_err(bad)?;
        }
        let receipt = ConnectionCommandReceipt {
            request,
            owner,
            kind: ConnectionCommandKind::Disconnect,
            fingerprint: fingerprint.to_owned(),
            planned_connection: None,
            connection: Some(connection),
            stage: ConnectionReceiptStage::Prepared,
            failure: None,
            expected_revision: Some(expected_revision),
            result_revision: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        insert_receipt(&tx, &receipt)?;
        let connection = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok((ConnectionCommandAdmission::Prepared(receipt), connection))
    }

    /// The last step of Disconnect: the tombstone, what the vendor said, and
    /// the completed receipt, in one transaction. The caller has cleared the
    /// credential slot. Grant rows that name the connection are kept; they
    /// point at a tombstone and authorize nothing.
    pub fn complete_disconnect(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        revoke: VendorRevoke,
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let receipt = open_receipt_on(&tx, owner, request, ConnectionCommandKind::Disconnect)?;
        let id = receipt.connection.ok_or(ConnectionError::Storage)?;
        let current = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        match receipt.stage {
            ConnectionReceiptStage::Completed => return Ok(current),
            ConnectionReceiptStage::Failed => return Err(ConnectionError::Conflict),
            ConnectionReceiptStage::Prepared => {}
        }
        if current.state != ConnectionState::Disconnecting {
            return Err(ConnectionError::Storage);
        }
        let revision = current.revision + 1;
        tx.execute(
            "UPDATE connections SET state='disconnected',vendor_revoke=?2,disconnected_at_ms=?3,revision=?4,updated_at_ms=MAX(updated_at_ms,?3) WHERE connection_id=?1",
            params![id.to_storage(), revoke.token(), now, revision],
        )
        .map_err(bad)?;
        finish_receipt(&tx, owner, request, None, Ok(revision), now)?;
        let connection = connection_on(&tx, owner, id)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }

    /// End a `prepared` command without its effect. Repeating it with the
    /// same reason returns the failed receipt; a completed command, or
    /// another reason, is a conflict. The connection row is not touched: a
    /// failed Disconnect leaves it `disconnecting`, which refuses every read
    /// and can be repeated.
    pub fn fail_connection_command(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        failure: ConnectionFailure,
        now: i64,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let receipt = receipt_on(&tx, owner, request)?.ok_or(ConnectionError::NotFound)?;
        match receipt.stage {
            ConnectionReceiptStage::Failed if receipt.failure == Some(failure) => {
                return Ok(receipt);
            }
            ConnectionReceiptStage::Prepared => {}
            _ => return Err(ConnectionError::Conflict),
        }
        finish_receipt(&tx, owner, request, None, Err(failure), now)?;
        let receipt = receipt_on(&tx, owner, request)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(receipt)
    }

    /// The vendor refused the stored credential, or the credential is gone:
    /// `connected` becomes `needs_sign_in` and the revision rises. Grants are
    /// kept. Any other state is returned unchanged.
    pub fn connection_needs_sign_in(
        &mut self,
        owner: BotOwner,
        connection: ConnectionId,
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let current = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        if current.state != ConnectionState::Connected {
            return Ok(current);
        }
        tx.execute(
            "UPDATE connections SET state='needs_sign_in',revision=revision+1,updated_at_ms=MAX(updated_at_ms,?2) WHERE connection_id=?1 AND state='connected'",
            params![connection.to_storage(), now],
        )
        .map_err(bad)?;
        let connection = connection_on(&tx, owner, connection)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }

    /// Record when the credential was last refreshed. Not a state change:
    /// the revision does not move.
    pub fn connection_refreshed(
        &mut self,
        owner: BotOwner,
        connection: ConnectionId,
        now: i64,
    ) -> Result<(), ConnectionError> {
        connection_on(&self.connection, owner, connection)?.ok_or(ConnectionError::NotFound)?;
        self.connection
            .execute(
                "UPDATE connections SET refreshed_at_ms=?2,updated_at_ms=MAX(updated_at_ms,?2) WHERE connection_id=?1 AND state IN ('connected','needs_sign_in')",
                params![connection.to_storage(), now],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// A read found another identity than the one the person picked (D17):
    /// the item becomes `changed` and the connection's revision rises. An
    /// item already `changed` is returned unchanged.
    pub fn connection_item_changed(
        &mut self,
        owner: BotOwner,
        item: ConnectionItemId,
        now: i64,
    ) -> Result<Connection, ConnectionError> {
        let (connection, found) = self
            .connection_item(owner, item)?
            .ok_or(ConnectionError::NotFound)?;
        if found.state == ConnectionItemState::Changed
            || connection.state == ConnectionState::Disconnected
        {
            return Ok(connection);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        tx.execute(
            "UPDATE connection_items SET state='changed',changed_at_ms=?2,updated_at_ms=MAX(updated_at_ms,?2) WHERE item_id=?1 AND state='available'",
            params![item.to_storage(), now],
        )
        .map_err(bad)?;
        tx.execute(
            "UPDATE connections SET revision=revision+1,updated_at_ms=MAX(updated_at_ms,?2) WHERE connection_id=?1",
            params![connection.id.to_storage(), now],
        )
        .map_err(bad)?;
        let connection =
            connection_on(&tx, owner, connection.id)?.ok_or(ConnectionError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(connection)
    }
}

/// One connect (or sign-in again) at a time per person.
fn connect_open_on(conn: &rusqlite::Connection, owner: BotOwner) -> Result<(), ConnectionError> {
    let open: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_command_receipts WHERE user_scope_id=?1 AND owner_device_id=?2 AND stage='prepared' AND command_kind='connect')",
            params![owner.user_scope.to_storage(), owner.device.to_storage()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if open {
        return Err(ConnectionError::Busy);
    }
    Ok(())
}

/// One unfinished command per connection.
fn command_open_on(
    conn: &rusqlite::Connection,
    connection: ConnectionId,
) -> Result<(), ConnectionError> {
    let open: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_command_receipts WHERE connection_id=?1 AND stage='prepared')",
            [connection.to_storage()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if open {
        return Err(ConnectionError::Busy);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
