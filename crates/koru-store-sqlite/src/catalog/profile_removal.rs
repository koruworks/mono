// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::ids::RemovalAttemptId;
use koru_store_core::profile_removal::{
    RemovalKind, RemovalMark, RemovalUpdate, removal_key, require_attempt,
};

pub(super) fn read(connection: &Connection, id: ProfileId) -> Result<Option<RemovalMark>> {
    let json: Option<String> = connection
        .query_row(
            "SELECT value FROM store_state WHERE key = ?1",
            params![removal_key(id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_failed)?;
    json.map(|value| {
        serde_json::from_str(&value)
            .map_err(|_| Error::new(Code::CorruptState, "invalid Profile removal record"))
    })
    .transpose()
}

pub(super) fn clear(connection: &Connection, id: ProfileId) -> Result<()> {
    connection
        .execute(
            "DELETE FROM store_state WHERE key = ?1",
            params![removal_key(id)],
        )
        .map_err(sql_failed)?;
    Ok(())
}

pub(super) fn update(
    connection: &mut Connection,
    id: ProfileId,
    update: RemovalUpdate,
) -> Result<Option<RemovalMark>> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_failed)?;
    let old = read(&tx, id)?;
    let mark = match update {
        RemovalUpdate::Begin { kind, at } => {
            let active: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM profiles WHERE id = ?1 AND removed_at IS NULL)",
                    params![id.to_storage()],
                    |row| row.get(0),
                )
                .map_err(sql_failed)?;
            if !active {
                return Err(Error::bare(Code::ProfileNotFound));
            }
            if old.as_ref().is_some_and(|mark| mark.lease_live(at)) {
                return Err(Error::bare(Code::RemovalInProgress));
            }
            let kind = if old
                .as_ref()
                .is_some_and(|mark| mark.kind == RemovalKind::HostedCleanup)
            {
                RemovalKind::HostedCleanup
            } else {
                kind
            };
            RemovalMark {
                attempt: RemovalAttemptId::new(),
                kind,
                started_at: at,
                last_step_at: at,
                stopped: false,
            }
        }
        RemovalUpdate::Stop { attempt } => {
            let mut mark = old
                .filter(|mark| mark.attempt == attempt)
                .ok_or_else(|| Error::bare(Code::RemovalAttemptStale))?;
            mark.stopped = true;
            mark
        }
        RemovalUpdate::Step { attempt, at } => {
            let mut mark = require_attempt(old.as_ref(), attempt)?.clone();
            mark.last_step_at = at;
            mark
        }
        RemovalUpdate::Keep { attempt } => {
            let mark = old
                .filter(|mark| mark.attempt == attempt)
                .ok_or_else(|| Error::bare(Code::RemovalAttemptStale))?;
            if mark.kind == RemovalKind::HostedCleanup {
                return Err(Error::bare(Code::RemovalKindHosted));
            }
            clear(&tx, id)?;
            tx.commit().map_err(sql_failed)?;
            return Ok(None);
        }
    };
    let json = serde_json::to_string(&mark).map_err(|_| Error::bare(Code::Internal))?;
    tx.execute("INSERT INTO store_state(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![removal_key(id), json]).map_err(sql_failed)?;
    tx.commit().map_err(sql_failed)?;
    Ok(Some(mark))
}

#[cfg(test)]
mod tests;
