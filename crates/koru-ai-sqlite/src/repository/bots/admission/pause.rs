// SPDX-License-Identifier: MPL-2.0
//! One pause receipt decoder for ordinary and authority-only readers.
use super::*;

pub(in crate::repository::bots) fn for_metadata(
    conn: &Connection,
    metadata: &BotPolicyMetadata,
    request: BotRequestId,
) -> Result<Option<BotPauseReceipt>, BotError> {
    // Read only this receipt's bounded fields; invalid non-NULL reserved fields
    // are errors, never silently projected as absent. No canonical folder read.
    let row = conn.query_row("SELECT
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))<=64 THEN fingerprint END,
        CASE WHEN typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=16 THEN command_kind END,
        CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
        CASE WHEN typeof(desired_pause)='integer' AND desired_pause IN (0,1) THEN desired_pause END,
        CASE WHEN typeof(expected_revision)='integer' AND expected_revision>0 THEN expected_revision END,
        CASE WHEN typeof(pause_ack_stage)='text' AND length(CAST(pause_ack_stage AS BLOB))<=16 THEN pause_ack_stage END,
        CASE WHEN typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB))<=1048576 THEN outcome_json END,
        planned_run_id IS NULL AND captured_config_revision IS NULL AND captured_policy_revision IS NULL
            AND submission_version IS NULL AND submission_json IS NULL
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
        params![metadata.owner.user_scope.to_storage(),request.to_storage()], |r| {
            if !r.get::<_,bool>(8)? { return Err(rusqlite::Error::InvalidQuery); }
            Ok(Raw { bot:r.get(0)?, fingerprint:r.get(1)?, kind:r.get(2)?, stage:r.get(3)?,
                desired:Some(r.get::<_,bool>(4)?),expected:Some(r.get::<_,u64>(5)?),
                ack:Some(r.get::<_,String>(6)?),outcome:Some(r.get::<_,String>(7)?),
                run:None,config:None,policy:None,version:None,submission:None })
        }).optional().map_err(bad)?;
    row.map(|r| {
        decode(r, metadata.owner.user_scope, request, |barrier| {
            if barrier.owner.user_scope != metadata.owner.user_scope
                || barrier.owner.device != metadata.owner.device
                || barrier.bot != metadata.id
                || metadata.lifecycle != BotLifecycle::Active
            {
                return Err(BotError::Forbidden);
            }
            Ok(())
        })
    })
    .transpose()
}

pub(super) fn decode(
    r: Raw,
    scope: UserScopeId,
    request: BotRequestId,
    current: impl FnOnce(&BotAdmissionBarrier) -> Result<(), BotError>,
) -> Result<BotPauseReceipt, BotError> {
    if !matches!(r.kind.as_str(), "pause" | "resume") {
        return Err(BotError::Conflict);
    }
    let data = r.outcome.ok_or(BotError::Storage)?;
    if data.len() > 1024 * 1024 {
        return Err(BotError::Storage);
    }
    let intent: BotPauseIntent = serde_json::from_str(&data).map_err(bad)?;
    intent.validate()?;
    let b = &intent.barrier;
    current(b)?;
    if b.owner.user_scope != scope
        || b.request != request
        || b.bot.to_storage() != r.bot
        || r.desired != Some(b.paused)
        || r.expected != Some(intent.expected_revision)
        || (r.kind == "pause") != b.paused
        || r.run.is_some()
        || r.submission.is_some()
    {
        return Err(BotError::Storage);
    }
    let acknowledged = match r.ack.as_deref() {
        Some("pending") => false,
        Some("acknowledged") => true,
        _ => return Err(BotError::Storage),
    };
    let stage = BotReceiptStage::parse(&r.stage).ok_or(BotError::Storage)?;
    // A pause its owner definitely refused is `rejected`, never
    // acknowledged; it no longer holds back the Bot's other changes.
    if (acknowledged && stage != BotReceiptStage::Completed)
        || (!acknowledged
            && !matches!(stage, BotReceiptStage::Prepared | BotReceiptStage::Rejected))
    {
        return Err(BotError::Storage);
    }
    Ok(BotPauseReceipt {
        intent,
        fingerprint: r.fingerprint,
        stage,
        acknowledged,
    })
}
