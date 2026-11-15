// SPDX-License-Identifier: MPL-2.0

//! Deny-only retained markers. A malformed value is not an ordinary negative.
use super::*;
use koru_delegate_core::BotProvenance;

fn bad(table: &'static str, column: &'static str) -> LedgerError {
    LedgerError::Corrupt { table, column }
}

/// Indexed deny-only marker projection; no JSON bodies are parsed. This is
/// sufficient for nonauthorizing control/history scalar projections only.
pub(crate) fn deny_run_columns(conn: &Connection, run: RunId) -> Result<(), LedgerError> {
    let version: Option<Option<i64>> = conn
        .query_row(
            "SELECT CASE WHEN typeof(submission_version)='integer' AND submission_version IN (1,2)
         THEN submission_version END FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    if version == Some(None) {
        return Err(bad("bot_admissions", "submission_version"));
    }
    let marked: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_routine_links WHERE run_id=?1)
        OR EXISTS(SELECT 1 FROM schedule_claims c JOIN bot_routine_routes r ON r.schedule_id=c.schedule_id WHERE c.run_id=?1)",
        [run.to_storage()], |r| r.get(0)).map_err(sql)?;
    if version == Some(Some(2)) || marked {
        return Err(LedgerError::InvalidScheduleAuthority);
    }
    Ok(())
}

pub(crate) fn deny_run_metadata(conn: &Connection, run: RunId) -> Result<(), LedgerError> {
    deny_run_columns(conn, run)?;
    let version: Option<i64> = conn
        .query_row(
            "SELECT submission_version FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    if version.is_none() {
        return Ok(());
    }

    // Only the provenance object is copied, never the task/context body. The
    // existing admission 4MiB domain bound precedes JSON extraction/allocation.
    // The owning strict BotProvenance decoder supplies identity/shape semantics.
    let provenance: Option<String> = conn
        .query_row(
            "SELECT CASE
        WHEN typeof(payload_json)='text' AND length(CAST(payload_json AS BLOB))<=4194304
        THEN CASE WHEN json_valid(payload_json) THEN CASE
          WHEN json_type(payload_json)='object' AND json_type(payload_json,'$.provenance')='object'
          THEN json_extract(payload_json,'$.provenance') END END END
        FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let p: BotProvenance =
        serde_json::from_str(&provenance.ok_or_else(|| bad("bot_admissions", "provenance"))?)
            .map_err(|_| bad("bot_admissions", "provenance"))?;
    if !p.valid() {
        return Err(bad("bot_admissions", "provenance"));
    }
    if p.routine.is_some() {
        return Err(LedgerError::InvalidScheduleAuthority);
    }
    let matches: bool = conn.query_row("SELECT typeof(config_revision)='integer' AND typeof(policy_revision)='integer'
        AND user_scope=?2 AND device_id=?3 AND bot_id=?4 AND request_id=?5 AND config_revision=?6 AND policy_revision=?7
        FROM bot_admissions WHERE run_id=?1", params![run.to_storage(),p.owner.user_scope.to_storage(),
        p.owner.device.to_storage(),p.bot.to_storage(),p.submission.to_storage(),p.config_revision,p.policy_revision], |r| r.get(0)).map_err(sql)?;
    if !matches {
        return Err(bad("bot_admissions", "provenance"));
    }
    Ok(())
}

pub(crate) fn deny_run(conn: &Connection, run: RunId) -> Result<(), LedgerError> {
    deny_run_metadata(conn, run)?;
    // Ordinary payloads have no aggregate byte limit. Project only closed scalar
    // origin fields, then use the existing RunOrigin decoder. Older missing
    // origin defaults to Manual; explicit null/malformed origin does not.
    let row = conn.query_row("SELECT
        CASE WHEN typeof(payload_json)='text' AND json_valid(payload_json) THEN json_type(payload_json)='object' ELSE 0 END,
        CASE WHEN json_valid(payload_json) THEN json_type(payload_json,'$.origin') IS NULL ELSE 0 END,
        CASE WHEN json_valid(payload_json) THEN
          CASE WHEN json_type(payload_json,'$.origin')='object' AND json_extract(payload_json,'$.origin.origin') IN ('manual','scheduled')
          THEN json_extract(payload_json,'$.origin.origin') END END,
        CASE WHEN json_valid(payload_json) THEN
          CASE WHEN json_type(payload_json,'$.origin.schedule')='text' AND length(CAST(json_extract(payload_json,'$.origin.schedule') AS BLOB))=36
          THEN json_extract(payload_json,'$.origin.schedule') END END,
        CASE WHEN json_valid(payload_json) THEN
          CASE WHEN json_type(payload_json,'$.origin.occurrence_ms')='integer' THEN json_extract(payload_json,'$.origin.occurrence_ms') END END,
        CASE WHEN json_valid(payload_json) THEN
          CASE WHEN json_type(payload_json,'$.origin.revision')='integer' THEN json_extract(payload_json,'$.origin.revision') END END
        FROM run_intents WHERE run_id=?1", [run.to_storage()], |r| Ok((r.get::<_,bool>(0)?,r.get::<_,bool>(1)?,
        r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<i64>>(5)?)))
        .optional().map_err(sql)?;
    let Some((object, missing, kind, schedule, occurrence_ms, revision)) = row else {
        return Ok(());
    };
    if !object {
        return Err(bad("run_intents", "payload_json"));
    }
    let origin = if missing {
        RunOrigin::Manual
    } else {
        serde_json::from_value::<RunOrigin>(serde_json::json!({"origin":kind,"schedule":schedule,
            "occurrence_ms":occurrence_ms,"revision":revision}))
        .map_err(|_| bad("run_intents", "origin"))?
    };
    if let RunOrigin::Scheduled { schedule, .. } = origin {
        deny_schedule(conn, schedule)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
