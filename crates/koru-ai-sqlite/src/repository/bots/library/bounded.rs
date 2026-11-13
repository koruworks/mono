// SPDX-License-Identifier: MPL-2.0
//! Bounded Library projections. Invalid rows remain errors, never missing rows.
use super::*;

pub(super) struct Receipt {
    pub bot: String,
    pub fingerprint: Option<String>,
    pub kind: String,
    pub stage: String,
    pub outcome: Option<String>,
    pub outcome_valid: bool,
}

/// Do not materialize submission JSON or unrelated receipt payloads. Header
/// validation precedes kind matching; a different valid command still conflicts
/// without having to decode that command's potentially much larger outcome.
pub(super) fn receipt(
    conn: &Connection,
    owner: UserScopeId,
    request: BotRequestId,
) -> Result<Option<Receipt>, BotError> {
    conn.query_row(
        "SELECT
         CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
         CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64 THEN fingerprint END,
         CASE WHEN typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=32 THEN command_kind END,
         CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
         CASE WHEN typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB))<=?3 THEN outcome_json END,
         outcome_json IS NULL OR (typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB))<=?3)
         FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
        params![owner.to_storage(), request.to_storage(), BOT_LIBRARY_RECEIPT_LIMIT],
        |row| Ok(Receipt {
            bot: row.get(0)?,
            fingerprint: row.get(1)?,
            kind: row.get(2)?,
            stage: row.get(3)?,
            outcome: row.get(4)?,
            outcome_valid: row.get(5)?,
        }),
    ).optional().map_err(bad)
}
