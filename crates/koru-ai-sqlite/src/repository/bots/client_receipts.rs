// SPDX-License-Identifier: MPL-2.0

//! The receipts of what a linked desktop asked for (M8, P8.3; ai.db 28).
//!
//! **One command, at most one effect.** Every command a linked device sends
//! is recorded here, as `applying`, in its own transaction, before anything
//! of it runs ([`Repository::client_receipt_begin`]). When it has run, the
//! row is closed with its outcome ([`Repository::client_receipt_finish`]).
//! A second arrival of the same request finds the row and is answered from
//! it; it never runs the command again:
//!
//! - closed: the stored outcome;
//! - `applying`, written by this start of the worker: the first arrival is
//!   still being applied;
//! - `applying`, written by an earlier start: that worker stopped between
//!   recording and finishing. The command is never applied again. The caller
//!   settles it from the command's own durable state where there is one
//!   ([`Repository::client_settle_bot_request`], the team pause row), and
//!   otherwise closes it as `unknown`.
//!
//! A row holds identities, a kind, a state and a code: no message, title,
//! task or response. A receipt write moves no feed marker.

use super::*;
use rusqlite::TransactionBehavior;
use uuid::Uuid;

/// A receipt older than this is deleted when a new one is written.
pub const CLIENT_RECEIPT_KEEP_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// At most this many old receipts are deleted beside one new one.
pub const CLIENT_RECEIPT_PRUNE_BATCH: i64 = 200;

/// How a recorded command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientOutcome {
    Completed,
    /// Durable and not finished: a pause waiting for its delegate.
    Accepted,
    Refused,
    /// Recorded, and the worker stopped before it recorded an outcome.
    /// Final.
    Unknown,
}

impl ClientOutcome {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Accepted => "accepted",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "completed" => Self::Completed,
            "accepted" => Self::Accepted,
            "refused" => Self::Refused,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// A closed receipt: what a repeat of the request is answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientReceipt {
    pub outcome: ClientOutcome,
    pub failure_code: Option<String>,
    /// The feed's head when the command had committed. Absent for a command
    /// that changed nothing.
    pub feed_cursor: Option<u64>,
}

/// What recording a command found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientReceiptBegin {
    /// No row existed. One was written as `applying` under this start:
    /// apply the command now, then finish the receipt.
    Apply,
    /// The request was closed before. Apply nothing.
    Done(ClientReceipt),
    /// The first arrival is still being applied by this start of the worker.
    /// Apply nothing and change nothing.
    InFlight,
    /// A worker that has since stopped recorded the request and never closed
    /// it. Apply nothing; settle it. `kind` is the kind that was recorded.
    Stranded { kind: String },
    /// The same request id with other content.
    Conflict,
}

/// What a Bot command's own receipt says of a request (`bot_command_receipts`,
/// keyed by the person and the request id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientBotRequest {
    pub kind: String,
    /// True while the change waits for its execution owner (`prepared`).
    pub pending: bool,
}

fn token32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn token64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn kind_valid(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

type Stored = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
);

fn stored_on(conn: &Connection, peer: DeviceId, request: Uuid) -> Result<Option<Stored>, BotError> {
    conn.query_row(
        "SELECT fingerprint, command_kind, boot_id, state, outcome, failure_code, feed_cursor
           FROM bot_client_receipts WHERE peer_device_id = ?1 AND request_id = ?2",
        params![peer.to_storage(), request.to_string()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )
    .optional()
    .map_err(bad)
}

fn closed(
    outcome: Option<String>,
    failure_code: Option<String>,
    feed_cursor: Option<i64>,
) -> Result<ClientReceipt, BotError> {
    Ok(ClientReceipt {
        outcome: outcome
            .as_deref()
            .and_then(ClientOutcome::parse)
            .ok_or(BotError::Storage)?,
        failure_code,
        feed_cursor: feed_cursor
            .map(|cursor| u64::try_from(cursor).map_err(bad))
            .transpose()?,
    })
}

impl Repository {
    /// Record one command of a linked device before it is applied, or find
    /// what an earlier arrival of the same request left (contract 4.4, step
    /// 1). One transaction.
    ///
    /// `fingerprint` is 64 hexadecimal digits of what the request asked for,
    /// `kind` its command kind, `boot_id` this start of the worker (the one
    /// [`Repository::feed_start`] recorded). When a row is written, up to
    /// [`CLIENT_RECEIPT_PRUNE_BATCH`] receipts older than
    /// [`CLIENT_RECEIPT_KEEP_MS`] are deleted with it, by `now_ms`.
    pub fn client_receipt_begin(
        &mut self,
        peer: DeviceId,
        request: Uuid,
        fingerprint: &str,
        kind: &str,
        boot_id: &str,
        now_ms: i64,
    ) -> Result<ClientReceiptBegin, BotError> {
        if !token64(fingerprint) || !token32(boot_id) || !kind_valid(kind) {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        if let Some((stored, stored_kind, stored_boot, state, outcome, failure_code, feed_cursor)) =
            stored_on(&tx, peer, request)?
        {
            if stored != fingerprint {
                return Ok(ClientReceiptBegin::Conflict);
            }
            return Ok(match state.as_str() {
                "done" => ClientReceiptBegin::Done(closed(outcome, failure_code, feed_cursor)?),
                "applying" if stored_boot == boot_id => ClientReceiptBegin::InFlight,
                "applying" => ClientReceiptBegin::Stranded { kind: stored_kind },
                _ => return Err(BotError::Storage),
            });
        }
        tx.execute(
            "DELETE FROM bot_client_receipts WHERE rowid IN (
               SELECT rowid FROM bot_client_receipts WHERE created_at_ms < ?1
                ORDER BY created_at_ms LIMIT ?2)",
            params![
                now_ms.saturating_sub(CLIENT_RECEIPT_KEEP_MS),
                CLIENT_RECEIPT_PRUNE_BATCH
            ],
        )
        .map_err(bad)?;
        tx.execute(
            "INSERT INTO bot_client_receipts(peer_device_id, request_id, fingerprint, command_kind,
               boot_id, state, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, 'applying', ?6, ?6)",
            params![
                peer.to_storage(),
                request.to_string(),
                fingerprint,
                kind,
                boot_id,
                now_ms
            ],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(ClientReceiptBegin::Apply)
    }

    /// Close a recorded command with its outcome (contract 4.4, step 3) and
    /// answer the closed receipt. A command that took effect (`completed` or
    /// `accepted`) is stamped with the feed's head as it is now, which is at
    /// or after the command's own commit; a refused or unknown one has no
    /// cursor. A receipt that is already closed is answered as it is and not
    /// rewritten: the first outcome stands.
    pub fn client_receipt_finish(
        &mut self,
        peer: DeviceId,
        request: Uuid,
        outcome: ClientOutcome,
        failure_code: Option<&str>,
        now_ms: i64,
    ) -> Result<ClientReceipt, BotError> {
        // A failure code is a short stable token. One that is not is
        // refused before anything is written, so the row is never closed
        // with text somebody typed.
        if failure_code.is_some_and(|code| {
            code.is_empty() || code.len() > 64 || !code.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(BotError::Invalid);
        }
        let effect = matches!(outcome, ClientOutcome::Completed | ClientOutcome::Accepted);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        tx.execute(
            "UPDATE bot_client_receipts SET state = 'done', outcome = ?3, failure_code = ?4,
               feed_cursor = CASE WHEN ?5 THEN (SELECT COALESCE(MAX(seq), 0) FROM bot_client_feed) END,
               updated_at_ms = ?6
             WHERE peer_device_id = ?1 AND request_id = ?2 AND state = 'applying'",
            params![
                peer.to_storage(),
                request.to_string(),
                outcome.token(),
                failure_code,
                effect,
                now_ms
            ],
        )
        .map_err(bad)?;
        let (_, _, _, state, outcome, failure_code, feed_cursor) =
            stored_on(&tx, peer, request)?.ok_or(BotError::NotFound)?;
        if state != "done" {
            return Err(BotError::Storage);
        }
        let receipt = closed(outcome, failure_code, feed_cursor)?;
        tx.commit().map_err(bad)?;
        Ok(receipt)
    }

    /// What a Bot command's own receipt holds for `request`, for settling a
    /// stranded `pause_bot`, `resume_bot`, `create_chat` or `update_bot`.
    /// Those commands write their receipt in the transaction of their
    /// effect, and nothing deletes one: a request with no row changed
    /// nothing.
    pub fn client_settle_bot_request(
        &self,
        user_scope: UserScopeId,
        request: Uuid,
    ) -> Result<Option<ClientBotRequest>, BotError> {
        self.connection
            .query_row(
                "SELECT command_kind, stage FROM bot_command_receipts
                  WHERE user_scope_id = ?1 AND request_id = ?2",
                params![user_scope.to_storage(), request.to_string()],
                |row| {
                    let stage: String = row.get(1)?;
                    Ok(ClientBotRequest {
                        kind: row.get(0)?,
                        pending: stage == "prepared",
                    })
                },
            )
            .optional()
            .map_err(bad)
    }

    /// Fixture only: every receipt, oldest first, as its kind, its state and
    /// its outcome.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn client_receipts_for_fixture(
        &self,
    ) -> Result<Vec<(String, String, Option<String>)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT command_kind, state, outcome FROM bot_client_receipts
                  ORDER BY created_at_ms, rowid",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        Ok(rows)
    }

    /// The Bot a conversation belongs to, when a copy on a linked desktop
    /// shows that conversation: it is a Bot's, and its marker is not
    /// removed (it is not private, and not one a Bot opened in another that
    /// its owner has not admitted). `None` for an ordinary chat, a hidden
    /// conversation and an unknown id alike, so a linked desktop's command
    /// cannot tell them apart.
    pub fn client_chat_bot(&self, chat: ChatId) -> Result<Option<BotId>, BotError> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT f.bot_id FROM bot_client_feed AS f
                   JOIN bot_chats AS bc ON bc.chat_id = f.subject_id AND bc.bot_id = f.bot_id
                  WHERE f.kind = 'chat' AND f.subject_id = ?1 AND f.removed = 0",
                [chat.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        value.map(|value| value.parse().map_err(bad)).transpose()
    }
}

#[cfg(test)]
mod tests;
