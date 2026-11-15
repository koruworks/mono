// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) fn validate_owner(owner: BotOwner, key: &BotWaitEnrollmentKey) -> Result<(), BotError> {
    if owner.user_scope != key.owner.user_scope || owner.device != key.owner.device {
        return Err(BotError::Forbidden);
    }
    key.validate().map_err(|_| BotError::Invalid)
}

fn decode_enrolled(json: &str) -> Result<BotWaitEnrollmentReply, BotError> {
    let receipt: BotWaitEnrollmentReply = serde_json::from_str(json).map_err(bad)?;
    receipt.validate().map_err(bad)?;
    if !matches!(receipt, BotWaitEnrollmentReply::Enrolled { .. }) {
        return Err(BotError::Storage);
    }
    Ok(receipt)
}

/// Indexed historical decision lookup for the replay owner. The same decoder
/// and normalized column crosschecks own both entry points.
pub(super) fn for_decision_on(
    conn: &Connection,
    decision: DecisionId,
) -> Result<Option<BotWaitEnrollmentReply>, BotError> {
    let body: Option<String> = conn.query_row(
        "SELECT CASE WHEN typeof(receipt_json)='text' AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 4096 THEN receipt_json END
         FROM bot_decision_wait_enrollments WHERE decision_id=?1", [decision.to_storage()], |r| r.get(0),
    ).optional().map_err(bad)?;
    let Some(body) = body else {
        return Ok(None);
    };
    let receipt = decode_enrolled(&body)?;
    let BotWaitEnrollmentReply::Enrolled { key, .. } = receipt else {
        return Err(BotError::Storage);
    };
    if key.decision_id != decision {
        return Err(BotError::Storage);
    }
    retained_on(conn, &key)
}

/// Read only after SQL type/byte preflight; the receipt is the sole decoder.
pub(super) fn retained_on(
    conn: &Connection,
    key: &BotWaitEnrollmentKey,
) -> Result<Option<BotWaitEnrollmentReply>, BotError> {
    let (count, bounded): (i64, Option<bool>) = conn.query_row(
        "SELECT count(*),min(typeof(receipt_json)='text' AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 4096
         AND typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36
         AND typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36
         AND typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36
         AND typeof(ai_attempt_id)='text' AND length(CAST(ai_attempt_id AS BLOB))=36
         AND typeof(origin_attempt_id)='text' AND length(CAST(origin_attempt_id AS BLOB))=36
         AND typeof(origin_session_id)='text' AND length(CAST(origin_session_id AS BLOB))=36
         AND typeof(origin_turn_id)='text' AND length(CAST(origin_turn_id AS BLOB))=36
         AND typeof(decision_revision)='integer' AND decision_revision>0
         AND typeof(wait_revision)='integer' AND wait_revision>0
         AND typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295
         AND typeof(receipt_digest)='blob' AND length(receipt_digest)=32 AND typeof(created_at_ms)='integer')
         FROM bot_decision_wait_enrollments WHERE wait_id=?1 OR decision_id=?2",
        params![key.wait_id.to_string(), key.decision_id.to_string()], |r| Ok((r.get(0)?,r.get(1)?)),
    ).map_err(bad)?;
    if count == 0 {
        return Ok(None);
    }
    if bounded != Some(true) {
        return Err(BotError::Storage);
    }
    // Each key is unique. Crosswiring two existing keys is a changed request,
    // while malformed SQL storage above still fails closed as corruption.
    if count == 2 {
        return Err(BotError::Conflict);
    }
    if count != 1 {
        return Err(BotError::Storage);
    }
    let json: String = conn.query_row(
        "SELECT receipt_json FROM bot_decision_wait_enrollments WHERE wait_id=?1 OR decision_id=?2",
        params![key.wait_id.to_string(),key.decision_id.to_string()], |r| r.get(0),
    ).map_err(bad)?;
    let receipt = decode_enrolled(&json)?;
    let BotWaitEnrollmentReply::Enrolled {
        key: stored,
        ai_attempt,
        receipt_digest,
    } = &receipt
    else {
        return Err(BotError::Storage);
    };
    let attempt = InferenceAttemptId::parse(&ai_attempt.to_string()).ok_or(BotError::Storage)?;
    let exact: bool = conn.query_row(
        "SELECT wait_id=?1 AND decision_id=?2 AND run_id=?3 AND ai_attempt_id=?4
         AND decision_revision=?5 AND wait_revision=?6 AND origin_attempt_id=?7
         AND origin_generation=?8 AND origin_session_id=?9 AND origin_turn_id=?10 AND receipt_digest=?11
         FROM bot_decision_wait_enrollments WHERE wait_id=?12 OR decision_id=?13",
        params![stored.wait_id.to_string(),stored.decision_id.to_string(),stored.run.to_storage(),attempt.to_storage(),
            stored.decision_revision.get(),stored.wait_revision.get(),stored.origin_attempt.to_storage(),stored.origin_generation.get(),
            stored.origin_session.to_string(),stored.origin_turn.to_string(),receipt_digest.as_bytes().as_slice(),key.wait_id.to_string(),key.decision_id.to_string()],
        |r| r.get(0),
    ).map_err(bad)?;
    if !exact {
        return Err(BotError::Storage);
    }
    if stored != key {
        return Err(BotError::Conflict);
    }
    Ok(Some(receipt))
}

pub(super) fn subject_on(
    conn: &Connection,
    owner: BotOwner,
    key: &BotWaitEnrollmentKey,
) -> Result<(InferenceAttemptId, BotSubmissionReceipt, BotReplayState), BotError> {
    let (receipt, _) = routines::model::retained_run_subject_on(conn, owner, key.bot, key.run)?;
    let p = &receipt.intent.submission.provenance;
    let attempt = InferenceAttemptId::parse(&p.ai_attempt_id.ok_or(BotError::Storage)?.to_string())
        .ok_or(BotError::Storage)?;
    // Bound every variable-width replay field before its existing decoder, and
    // prove the retained association reaches the actual assistant and attempt.
    let bounded: bool = conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(l.user_scope_id)='text' AND length(CAST(l.user_scope_id AS BLOB))=36
         AND typeof(l.owner_device_id)='text' AND length(CAST(l.owner_device_id AS BLOB))=36
         AND typeof(l.bot_id)='text' AND length(CAST(l.bot_id AS BLOB))=36
         AND typeof(l.run_id)='text' AND length(CAST(l.run_id AS BLOB))=36
         AND typeof(r.run_attempt_id)='text' AND length(CAST(r.run_attempt_id AS BLOB))=36
         AND typeof(r.generation)='integer' AND r.generation BETWEEN 1 AND 4294967295
         AND typeof(r.session_id)='text' AND length(CAST(r.session_id AS BLOB))=36
         AND (r.turn_id IS NULL OR (typeof(r.turn_id)='text' AND length(CAST(r.turn_id AS BLOB))=36))
         AND (r.last_sequence IS NULL OR (typeof(r.last_sequence)='integer' AND r.last_sequence>=0))
         AND (r.stop_reason IS NULL OR (typeof(r.stop_reason)='text' AND length(CAST(r.stop_reason AS BLOB))<=32))),0)
         FROM bot_chat_runs l JOIN bot_run_replay r USING(ai_attempt_id)
         JOIN bot_chats c ON c.chat_id=l.chat_id AND c.bot_id=l.bot_id
         JOIN inference_attempts a ON a.id=l.ai_attempt_id AND a.chat_id=l.chat_id AND a.assistant_message_id=l.assistant_message_id
         JOIN messages m ON m.id=l.assistant_message_id AND m.chat_id=l.chat_id AND m.role='assistant'
         WHERE l.ai_attempt_id=?1", [attempt.to_storage()], |r| r.get(0),
    ).map_err(bad)?;
    if !bounded {
        return Err(BotError::Storage);
    }
    let replay =
        crate::repository::bot_replay::replay_on(conn, attempt).map_err(|_| BotError::Storage)?;
    let fence = replay.fence.as_ref().ok_or(BotError::Storage)?;
    if fence.owner != key.owner || fence.bot != key.bot || fence.run != key.run {
        return Err(BotError::Conflict);
    }
    Ok((attempt, receipt, replay))
}

fn require_current_origin(
    replay: &BotReplayState,
    key: &BotWaitEnrollmentKey,
) -> Result<(), BotError> {
    let fence = replay.fence.as_ref().ok_or(BotError::Storage)?;
    if fence.attempt != key.origin_attempt
        || fence.generation != key.origin_generation
        || fence.session != key.origin_session
        || fence.turn != Some(key.origin_turn)
    {
        return Err(BotError::Conflict);
    }
    Ok(())
}

impl Repository {
    /// Historical lookup proves the exact retained subject without current
    /// model, card or execution policy. Absence is successful only after proof.
    pub fn read_bot_wait_cleanup(
        &self,
        owner: BotOwner,
        request: &ReadBotWaitCleanup,
    ) -> Result<BotWaitEnrollmentReply, BotError> {
        validate_owner(owner, &request.key)?;
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        let prior = retained_on(&tx, &request.key)?;
        let (attempt, _, replay) = subject_on(&tx, owner, &request.key)?;
        if let Some(prior) = prior {
            if let BotWaitEnrollmentReply::Enrolled { ai_attempt, .. } = &prior {
                if *ai_attempt != attempt.as_uuid() {
                    return Err(BotError::Storage);
                }
            }
            return Ok(prior);
        }
        require_current_origin(&replay, &request.key)?;
        Ok(BotWaitEnrollmentReply::Missing {
            key: request.key.clone(),
        })
    }

    /// `qualified` must come from the caller's independently authenticated
    /// delegate observation. Its DTO shape is neither a signature nor authority.
    /// External authentication/work occurs before this writer transaction.
    pub fn enroll_bot_wait_cleanup(
        &mut self,
        owner: BotOwner,
        request: &EnrollBotWaitCleanup,
        qualified: &BotDecisionWaitObservation,
        now: Timestamp,
    ) -> Result<BotWaitEnrollmentReply, BotError> {
        validate_owner(owner, &request.key)?;
        request.validate().map_err(|_| BotError::Invalid)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let prior = retained_on(&tx, &request.key)?;
        let (attempt, retained, replay) = subject_on(&tx, owner, &request.key)?;
        if let Some(prior) = prior {
            if let BotWaitEnrollmentReply::Enrolled { ai_attempt, .. } = &prior {
                if *ai_attempt != attempt.as_uuid() {
                    return Err(BotError::Storage);
                }
            }
            return Ok(prior);
        }
        require_current_origin(&replay, &request.key)?;
        qualified.validate().map_err(|_| BotError::Invalid)?;
        if qualified.key != request.key || qualified.cleanup_intent != request.cleanup_intent {
            return Err(BotError::Conflict);
        }
        if qualified.fresh_stop_eligibility != FreshWaitStopEligibility::EligibleOriginWaitStop
            || qualified.current_revision != request.key.wait_revision
            || !matches!(&qualified.disposition, DecisionDisposition::Open { state } if state.kind == DecisionKind::KoruEdit && state.authorization == DecisionAuthorization::Pending && state.phase == DecisionPhase::Holding && state.revision == request.key.decision_revision)
            || !matches!(qualified.publication, DecisionPublication::Applied { .. })
            || !matches!(qualified.origin_stop, OriginWaitStop::Pending {})
            || !matches!(qualified.consumer_cleanup, ConsumerCleanup::NotRequested {})
        {
            return Err(BotError::RevisionConflict);
        }
        let original_fence = match &retained.outcome {
            Some(BotExecutionOutcome::Accepted { admission, .. }) => {
                admission.fence.as_ref().ok_or(BotError::Storage)?
            }
            _ => return Err(BotError::Conflict),
        };
        if !matches!(
            retained.stage,
            BotReceiptStage::Accepted | BotReceiptStage::Completed
        ) || replay.stop_reason.is_some()
        {
            return Err(BotError::Conflict);
        }
        if Some(original_fence) != replay.fence.as_ref()
            && replay::installed_consumer_on(
                &tx,
                attempt,
                replay.fence.as_ref().ok_or(BotError::Storage)?,
            )?
            .is_none()
        {
            return Err(BotError::Conflict);
        }
        let live: bool = tx.query_row(
            "SELECT status IN ('pending','streaming') AND stop_reason IS NULL FROM inference_attempts WHERE id=?1",
            [attempt.to_storage()], |r| r.get(0),
        ).map_err(bad)?;
        if !live {
            return Err(BotError::Conflict);
        }
        let reply = BotWaitEnrollmentReply::Enrolled {
            key: request.key.clone(),
            ai_attempt: attempt.as_uuid(),
            receipt_digest: request.key.receipt_digest,
        };
        reply.validate().map_err(bad)?;
        let json =
            String::from_utf8(encode_decision_value(&reply, DECISION_EVIDENCE_BYTES).map_err(bad)?)
                .map_err(bad)?;
        let k = &request.key;
        tx.execute("INSERT INTO bot_decision_wait_enrollments(wait_id,decision_id,run_id,ai_attempt_id,decision_revision,wait_revision,origin_attempt_id,origin_generation,origin_session_id,origin_turn_id,receipt_digest,receipt_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![k.wait_id.to_string(),k.decision_id.to_string(),k.run.to_storage(),attempt.to_storage(),k.decision_revision.get(),k.wait_revision.get(),k.origin_attempt.to_storage(),k.origin_generation.get(),k.origin_session.to_string(),k.origin_turn.to_string(),k.receipt_digest.as_bytes().as_slice(),json,now.millis()]).map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(reply)
    }
}

#[cfg(test)]
pub(super) mod tests;
