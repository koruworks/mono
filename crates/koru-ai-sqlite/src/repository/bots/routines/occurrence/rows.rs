// SPDX-License-Identifier: MPL-2.0
//! Bounded row decoding with repeated SQL/JSON/receipt identities checked.
use super::*;

pub(super) fn read(
    conn: &Connection,
    slot: &RoutineSlot,
) -> Result<Option<BotRoutineStoredPreparation>, BotError> {
    if !slot.valid() {
        return Err(BotError::Invalid);
    }
    let row = conn.query_row("SELECT
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        definition_revision,
        CASE WHEN typeof(definition_digest)='blob' AND length(definition_digest)=32 THEN definition_digest END,
        CASE WHEN owner_handoff IS NULL OR (typeof(owner_handoff)='text' AND length(CAST(owner_handoff AS BLOB))=36) THEN owner_handoff ELSE 1 END,
        CASE WHEN typeof(semantic_hash)='blob' AND length(semantic_hash)=32 THEN semantic_hash END,
        CASE WHEN typeof(preparation_hash)='blob' AND length(preparation_hash)=32 THEN preparation_hash END,
        CASE WHEN typeof(submission_request_id)='text' AND length(CAST(submission_request_id AS BLOB))=36 THEN submission_request_id END,
        CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END,
        CASE WHEN typeof(chat_id)='text' AND length(CAST(chat_id AS BLOB))=36 THEN chat_id END,
        CASE WHEN typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36 THEN attempt_id END,
        CASE WHEN typeof(user_message_id)='text' AND length(CAST(user_message_id AS BLOB))=36 THEN user_message_id END,
        CASE WHEN typeof(assistant_message_id)='text' AND length(CAST(assistant_message_id AS BLOB))=36 THEN assistant_message_id END,
        config_revision,policy_revision,grant_revision,
        CASE WHEN typeof(submission_json)='text' AND length(CAST(submission_json AS BLOB)) BETWEEN 1 AND 4194304 THEN submission_json END,
        CASE WHEN typeof(capture_json)='text' AND length(CAST(capture_json AS BLOB)) BETWEEN 1 AND 1048576 THEN capture_json END,
        CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
        CASE WHEN linked_request_id IS NULL OR (typeof(linked_request_id)='text' AND length(CAST(linked_request_id AS BLOB))=36) THEN linked_request_id ELSE 1 END,
        CASE WHEN linked_chat_id IS NULL OR (typeof(linked_chat_id)='text' AND length(CAST(linked_chat_id AS BLOB))=36) THEN linked_chat_id ELSE 1 END,
        CASE WHEN close_reason IS NULL OR (typeof(close_reason)='text' AND length(CAST(close_reason AS BLOB))<=32) THEN close_reason ELSE 1 END,
        created_at_ms,updated_at_ms
        FROM bot_routine_preparations WHERE user_scope_id=?1 AND owner_device_id=?2 AND schedule_id=?3 AND occurrence_ms=?4",
        params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.schedule.to_storage(),slot.occurrence_ms], |r| {
            let text = |i| r.get::<_,String>(i);
            Ok((text(0)?,r.get::<_,u32>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Vec<u8>>(4)?,r.get::<_,Vec<u8>>(5)?,
                text(6)?,text(7)?,text(8)?,text(9)?,text(10)?,text(11)?,r.get::<_,u64>(12)?,r.get::<_,u64>(13)?,r.get::<_,u64>(14)?,
                text(15)?,text(16)?,text(17)?,r.get::<_,Option<String>>(18)?,r.get::<_,Option<String>>(19)?,r.get::<_,Option<String>>(20)?,r.get::<_,i64>(21)?,r.get::<_,i64>(22)?))
        }).optional().map_err(bad)?;
    let Some(r) = row else {
        return Ok(None);
    };
    if r.0 != slot.bot.to_storage() {
        return Err(BotError::Forbidden);
    }
    let intent: BotSubmissionIntent = serde_json::from_str(&r.15).map_err(bad)?;
    intent
        .validate_for_kind(BotOperationKind::RoutineOccurrence)
        .map_err(|_| BotError::Storage)?;
    let capture: BotRoutineCaptureV1 = serde_json::from_str(&r.16).map_err(bad)?;
    let digest = |bytes: Vec<u8>| {
        bytes
            .try_into()
            .map(RoutineDigest32::from_bytes)
            .map_err(|_| BotError::Storage)
    };
    let prepared = RoutinePrepared {
        slot: *slot,
        definition: RoutineDefinitionIdentity {
            revision: r.1,
            digest: digest(r.2)?,
            owner_handoff: r.3.map(|s| s.parse().map_err(bad)).transpose()?,
        },
        ids: RoutineExecutionIds {
            submission: r.6.parse().map_err(bad)?,
            run: r.7.parse().map_err(bad)?,
            chat: r.8.parse().map_err(bad)?,
            attempt: r.9.parse().map_err(bad)?,
            user_message: r.10.parse().map_err(bad)?,
            assistant_message: r.11.parse().map_err(bad)?,
        },
        semantic_hash: digest(r.4)?,
        preparation_hash: digest(r.5)?,
        submission_version: 2,
        submission: intent.submission,
    };
    if (r.12, r.13, r.14)
        != (
            capture.canonical.config_revision,
            capture.canonical.policy_revision,
            capture.grant.grant_revision,
        )
    {
        return Err(BotError::Storage);
    }
    let mut row = BotRoutineStoredPreparation {
        prepared,
        capture,
        stage: BotRoutineStoredStage::Prepared,
        created_at_ms: r.21,
        updated_at_ms: r.22,
    };
    row.validate().map_err(|_| BotError::Storage)?;
    let p = &row.prepared;
    let (kind,stage,fingerprint,outcome):(String,String,String,Option<String>) = conn.query_row("SELECT
        CASE WHEN typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=32 THEN command_kind END,
        CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
        CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64 THEN fingerprint END,
        CASE WHEN outcome_json IS NULL OR (typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 1048576) THEN outcome_json ELSE 1 END
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2 AND bot_id=?3",
        params![slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),slot.bot.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(bad)?;
    if fingerprint != String::from(p.semantic_hash) {
        return Err(BotError::Storage);
    }
    if r.17 == "linked" {
        if kind != "routine_occurrence"
            || !matches!(
                stage.as_str(),
                "prepared" | "accepted" | "completed" | "rejected"
            )
            || r.18.as_deref() != Some(p.ids.submission.to_storage().as_str())
            || r.19.as_deref() != Some(p.ids.chat.to_string().as_str())
            || r.20.is_some()
        {
            return Err(BotError::Storage);
        }
        // Existing receipt decoder validates stage/outcome; guard its largest
        // allocation in SQL first, including malicious rows with CHECK disabled.
        let exact:bool = conn.query_row("SELECT submission_version=2 AND planned_run_id=?3 AND captured_config_revision=?4 AND captured_policy_revision=?5 AND typeof(submission_json)='text' AND length(CAST(submission_json AS BLOB)) BETWEEN 1 AND 4194304 AND expected_revision IS NULL AND desired_pause IS NULL AND pause_ack_stage IS NULL FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
            params![slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),p.ids.run.to_storage(),r.12,r.13], |r| r.get(0)).map_err(bad)?;
        if !exact {
            return Err(BotError::Storage);
        }
        let receipt = admission::decode_submission(
            admission::raw(conn, slot.owner.user_scope, p.ids.submission)?
                .ok_or(BotError::Storage)?,
            slot.owner.user_scope,
            p.ids.submission,
        )?;
        if receipt.intent.submission != p.submission {
            return Err(BotError::Storage);
        }
        let associated:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_chats c JOIN chats h ON h.id=c.chat_id JOIN bot_chat_runs b ON b.chat_id=c.chat_id AND b.bot_id=c.bot_id JOIN inference_attempts a ON a.id=b.ai_attempt_id JOIN messages u ON u.id=?11 JOIN messages m ON m.id=b.assistant_message_id WHERE c.bot_id=?1 AND c.chat_id=?2 AND b.user_scope_id=?3 AND b.owner_device_id=?4 AND b.submission_id=?5 AND b.run_id=?6 AND b.ai_attempt_id=?7 AND b.assistant_message_id=?8 AND b.record_workspace_id=?9 AND b.continued_from_run_id IS NULL AND a.operation_id=?10 AND a.chat_id=c.chat_id AND a.assistant_message_id=b.assistant_message_id AND u.chat_id=c.chat_id AND u.role='user' AND m.chat_id=c.chat_id AND m.role='assistant')",
            params![slot.bot.to_storage(),p.ids.chat.to_string(),slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),p.ids.submission.to_storage(),p.ids.run.to_storage(),p.ids.attempt.to_string(),p.ids.assistant_message.to_string(),p.submission.workspace.to_storage(),p.ids.submission.as_uuid().to_string(),p.ids.user_message.to_string()],|r|r.get(0)).map_err(bad)?;
        if !associated {
            return Err(BotError::Storage);
        }
        let profile = match row.capture.schedule_definition.template.requested_by {
            koru_delegate_core::ActorRef::Person { profile } => profile,
            _ => return Err(BotError::Storage),
        };
        let exact_profile: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM chats WHERE id=?1 AND profile_id=?2)",
                params![p.ids.chat.to_string(), profile.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if !exact_profile {
            return Err(BotError::Storage);
        }
        if library::library_capture_on(
            conn,
            owner(slot),
            slot.bot,
            p.ids.submission,
            row.capture.canonical.home,
        )?
        .as_ref()
            != Some(&row.capture.library)
        {
            return Err(BotError::Storage);
        }
        row.stage = BotRoutineStoredStage::Linked;
    } else {
        if kind != "routine_draft" || r.18.is_some() || r.19.is_some() {
            return Err(BotError::Storage);
        }
        let exact:bool=conn.query_row("SELECT planned_run_id IS NULL AND captured_config_revision IS NULL AND captured_policy_revision IS NULL AND submission_version IS NULL AND submission_json IS NULL AND expected_revision IS NULL AND desired_pause IS NULL AND pause_ack_stage IS NULL FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",params![slot.owner.user_scope.to_storage(),p.ids.submission.to_storage()],|r|r.get(0)).map_err(bad)?;
        if !exact {
            return Err(BotError::Storage);
        }
        let outcome = outcome.ok_or(BotError::Storage)?;
        if outcome.len() > BOT_ROUTINE_RECEIPT_BYTES {
            return Err(BotError::Storage);
        }
        let receipt: DraftReceipt = serde_json::from_str(&outcome).map_err(bad)?;
        if !receipt.matches(&row) {
            return Err(BotError::Storage);
        }
        match (
            r.17.as_str(),
            stage.as_str(),
            receipt.closure,
            r.20.as_deref(),
        ) {
            ("prepared", "prepared", None, None) => {}
            ("closed", "rejected", Some(reason), Some(sql))
                if sql == close_name(reason.sql_reason()) =>
            {
                row.stage = BotRoutineStoredStage::Closed(reason)
            }
            _ => return Err(BotError::Storage),
        }
    }
    Ok(Some(row))
}

pub(super) fn insert(conn: &Connection, row: &BotRoutineStoredPreparation) -> Result<(), BotError> {
    row.validate()?;
    let p = &row.prepared;
    let slot = &p.slot;
    let outcome = serde_json::to_string(&DraftReceipt::for_row(row)).map_err(bad)?;
    let intent = BotSubmissionIntent {
        version: 2,
        submission: p.submission.clone(),
    };
    intent.validate_for_kind(BotOperationKind::RoutineOccurrence)?;
    conn.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'routine_draft','prepared',?5,?6,?6)",
        params![slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),slot.bot.to_storage(),String::from(p.semantic_hash),outcome,row.created_at_ms]).map_err(bad)?;
    conn.execute("INSERT INTO bot_routine_preparations(user_scope_id,owner_device_id,bot_id,schedule_id,occurrence_ms,definition_revision,definition_digest,owner_handoff,semantic_hash,preparation_hash,submission_request_id,run_id,chat_id,attempt_id,user_message_id,assistant_message_id,config_revision,policy_revision,grant_revision,submission_json,capture_json,stage,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,'prepared',?22,?22)",
        params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage(),slot.schedule.to_storage(),slot.occurrence_ms,p.definition.revision,p.definition.digest.as_bytes().as_slice(),p.definition.owner_handoff.map(|id|id.to_storage()),p.semantic_hash.as_bytes().as_slice(),p.preparation_hash.as_bytes().as_slice(),p.ids.submission.to_storage(),p.ids.run.to_storage(),p.ids.chat.to_string(),p.ids.attempt.to_string(),p.ids.user_message.to_string(),p.ids.assistant_message.to_string(),row.capture.canonical.config_revision,row.capture.canonical.policy_revision,row.capture.grant.grant_revision,serde_json::to_string(&intent).map_err(bad)?,serde_json::to_string(&row.capture).map_err(bad)?,row.created_at_ms]).map_err(bad)?;
    Ok(())
}

pub(super) fn close(
    conn: &Connection,
    row: &mut BotRoutineStoredPreparation,
    closure: BotRoutineStoredClosure,
    now: i64,
) -> Result<(), BotError> {
    if row.stage != BotRoutineStoredStage::Prepared {
        return Err(BotError::Conflict);
    }
    row.stage = BotRoutineStoredStage::Closed(closure);
    row.updated_at_ms = now.max(row.updated_at_ms);
    let p = &row.prepared;
    let changed=conn.execute("UPDATE bot_routine_preparations SET stage='closed',close_reason=?5,updated_at_ms=?6 WHERE user_scope_id=?1 AND owner_device_id=?2 AND schedule_id=?3 AND occurrence_ms=?4 AND stage='prepared' AND preparation_hash=?7",
        params![p.slot.owner.user_scope.to_storage(),p.slot.owner.device.to_storage(),p.slot.schedule.to_storage(),p.slot.occurrence_ms,close_name(closure.sql_reason()),row.updated_at_ms,p.preparation_hash.as_bytes().as_slice()]).map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Conflict);
    }
    let changed=conn.execute("UPDATE bot_command_receipts SET stage='rejected',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2 AND command_kind='routine_draft' AND stage='prepared'",
        params![p.slot.owner.user_scope.to_storage(),p.ids.submission.to_storage(),serde_json::to_string(&DraftReceipt::for_row(row)).map_err(bad)?,row.updated_at_ms]).map_err(bad)?;
    if changed != 1 {
        return Err(BotError::Conflict);
    }
    Ok(())
}
