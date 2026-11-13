// SPDX-License-Identifier: MPL-2.0

//! Retained routing markers deny ordinary execution; they never authorize it.
use crate::{BotLedgerError, LedgerError, RunsDb, WriteOp};
use koru_delegate_core::{
    DelegationRequest, RoutineDigest32, RoutineRouteIdentity, RunId, RunOrigin, ScheduleId,
};
use rusqlite::{Connection, OptionalExtension, params};

fn sql(error: rusqlite::Error) -> LedgerError {
    LedgerError::sql("access Routine routing", error)
}
fn corrupt() -> LedgerError {
    LedgerError::Corrupt {
        table: "bot_routine_routes",
        column: "identity",
    }
}
fn parse<T: std::str::FromStr>(value: String) -> Result<T, LedgerError> {
    value.parse().map_err(|_| corrupt())
}
fn read(
    conn: &Connection,
    schedule: ScheduleId,
) -> Result<Option<RoutineRouteIdentity>, LedgerError> {
    // Bound corrupt database cells before allocating their Rust representations.
    conn.query_row("SELECT
        CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,
        CASE WHEN typeof(device_id)='text' AND length(CAST(device_id AS BLOB))=36 THEN device_id END,
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
        CASE WHEN typeof(creation_request_id)='text' AND length(CAST(creation_request_id AS BLOB))=36 THEN creation_request_id END,
        CASE WHEN typeof(seed_hash)='blob' AND length(seed_hash)=32 THEN seed_hash END,
        created_at_ms FROM bot_routine_routes WHERE schedule_id=?1", [schedule.to_storage()], |row| {
        Ok((|| {
            let identity = RoutineRouteIdentity {
                owner: koru_delegate_core::BotOwner {
                    user_scope: serde_json::from_value(serde_json::Value::String(row.get(0).map_err(sql)?)).map_err(|_| corrupt())?,
                    device: koru_delegate_core::DeviceId::parse(&row.get::<_,String>(1).map_err(sql)?).ok_or_else(corrupt)?,
                },
                bot: parse(row.get(2).map_err(sql)?)?, schedule,
                workspace: parse(row.get(3).map_err(sql)?)?,
                creation_request: parse(row.get(4).map_err(sql)?)?,
                seed_hash: RoutineDigest32::from_bytes(row.get::<_,Vec<u8>>(5).map_err(sql)?.try_into().map_err(|_| corrupt())?),
                created_at_ms: row.get(6).map_err(sql)?,
            };
            if !identity.valid() { return Err(corrupt()); }
            Ok(identity)
        })())
    }).optional().map_err(sql)?.transpose()
}

impl RunsDb {
    /// Immutable registration. An exact replay returns the original identity.
    /// No worker, claim, admission, or executable Routine is enabled here.
    pub fn register_routine_route(
        &mut self,
        identity: &RoutineRouteIdentity,
    ) -> Result<RoutineRouteIdentity, BotLedgerError> {
        if !identity.valid() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::RegisterRoutineRoute, |tx, _now| {
            if let Some(old) = read(tx, identity.schedule)? {
                return if old == *identity { Ok(old) } else { Err(BotLedgerError::Conflict) };
            }
            tx.execute("INSERT INTO bot_routine_routes(schedule_id,user_scope,device_id,bot_id,workspace_id,creation_request_id,seed_hash,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![identity.schedule.to_storage(),identity.owner.user_scope.to_storage(),identity.owner.device.to_storage(),identity.bot.to_storage(),identity.workspace.to_storage(),identity.creation_request.to_storage(),identity.seed_hash.as_bytes().as_slice(),identity.created_at_ms]).map_err(sql)?;
            Ok(identity.clone())
        })
    }

    pub fn routine_route(
        &self,
        schedule: ScheduleId,
    ) -> Result<Option<RoutineRouteIdentity>, LedgerError> {
        read(self.conn(), schedule)
    }
}

pub(crate) fn deny_schedule(conn: &Connection, schedule: ScheduleId) -> Result<(), LedgerError> {
    // Existence alone is sufficient for denial, even if stored identity is corrupt.
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_routine_routes WHERE schedule_id=?1)",
            [schedule.to_storage()],
            |row| row.get(0),
        )
        .map_err(sql)?;
    if exists {
        return Err(LedgerError::InvalidScheduleAuthority);
    }
    Ok(())
}
pub(crate) fn deny_request(
    conn: &Connection,
    request: &DelegationRequest,
) -> Result<(), LedgerError> {
    if let RunOrigin::Scheduled { schedule, .. } = request.origin {
        deny_schedule(conn, schedule)?;
    }
    Ok(())
}
pub(crate) fn deny_run_metadata(conn: &Connection, run: RunId) -> Result<(), LedgerError> {
    let blocked: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE run_id=?1 AND submission_version<>1) OR EXISTS(SELECT 1 FROM schedule_claims c JOIN bot_routine_routes r ON r.schedule_id=c.schedule_id WHERE c.run_id=?1)", [run.to_storage()], |row| row.get(0)).map_err(sql)?;
    if blocked {
        return Err(LedgerError::InvalidScheduleAuthority);
    }
    Ok(())
}
pub(crate) fn deny_run(conn: &Connection, run: RunId) -> Result<(), LedgerError> {
    deny_run_metadata(conn, run)?;
    // Ordinary requests have no aggregate stored-byte bound. Project only the
    // marker relationship, without copying or decoding their content in Rust.
    let marked: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM run_intents i JOIN bot_routine_routes r ON r.schedule_id=json_extract(i.payload_json,'$.origin.schedule') WHERE i.run_id=?1)",[run.to_storage()],|row|row.get(0)).map_err(sql)?;
    if marked {
        return Err(LedgerError::InvalidScheduleAuthority);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
