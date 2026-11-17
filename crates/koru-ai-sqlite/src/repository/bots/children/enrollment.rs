// SPDX-License-Identifier: MPL-2.0
//! The AI owner's enrollment of a child wait: one immutable row per wait.
//!
//! Two callers write it and cannot disagree. The worker writes it from the
//! intent the delegate returned with the ask, before the model sees the
//! tool's answer. The delegate's enroll route writes it from the intent the
//! delegate holds, when the worker's write was lost. Both go through
//! [`enroll_on`]: insert if absent, else require the same receipt digest. The
//! digest is the frozen intent's, written at wait revision 1; the wait's
//! current revision is never compared, because the wait may have moved by the
//! time the second writer arrives.
use super::*;

fn validate_owner(owner: BotOwner, key: &BotChildWaitEnrollmentKey) -> Result<(), BotError> {
    if owner.user_scope != key.owner.user_scope || owner.device != key.owner.device {
        return Err(BotError::Forbidden);
    }
    key.validate().map_err(|_| BotError::Invalid)
}

fn decode_enrolled(json: &str) -> Result<BotChildWaitEnrollmentReply, BotError> {
    let receipt: BotChildWaitEnrollmentReply = serde_json::from_str(json).map_err(bad)?;
    receipt.validate().map_err(bad)?;
    if !matches!(receipt, BotChildWaitEnrollmentReply::Enrolled { .. }) {
        return Err(BotError::Storage);
    }
    Ok(receipt)
}

/// The retained enrollment of a wait, or none. Read only after an SQL type
/// and size preflight; the receipt is the sole decoder, and every normalized
/// column is checked against it.
pub(in crate::repository::bots) fn retained_on(
    conn: &Connection,
    wait: BotWaitId,
) -> Result<Option<(BotChildWaitEnrollmentKey, InferenceAttemptId)>, BotError> {
    let (count, bounded): (i64, Option<bool>) = conn
        .query_row(
            "SELECT count(*),min(typeof(receipt_json)='text' AND length(CAST(receipt_json AS BLOB)) BETWEEN 1 AND 4096
             AND typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36
             AND typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36
             AND typeof(ai_attempt_id)='text' AND length(CAST(ai_attempt_id AS BLOB))=36
             AND typeof(origin_attempt_id)='text' AND length(CAST(origin_attempt_id AS BLOB))=36
             AND typeof(origin_session_id)='text' AND length(CAST(origin_session_id AS BLOB))=36
             AND typeof(origin_turn_id)='text' AND length(CAST(origin_turn_id AS BLOB))=36
             AND typeof(wait_revision)='integer' AND wait_revision>0
             AND typeof(origin_generation)='integer' AND origin_generation BETWEEN 1 AND 4294967295
             AND typeof(receipt_digest)='blob' AND length(receipt_digest)=32 AND typeof(created_at_ms)='integer')
             FROM bot_child_wait_enrollments WHERE wait_id=?1",
            [wait.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(bad)?;
    if count == 0 {
        return Ok(None);
    }
    if count != 1 || bounded != Some(true) {
        return Err(BotError::Storage);
    }
    let json: String = conn
        .query_row(
            "SELECT receipt_json FROM bot_child_wait_enrollments WHERE wait_id=?1",
            [wait.to_string()],
            |r| r.get(0),
        )
        .map_err(bad)?;
    let BotChildWaitEnrollmentReply::Enrolled {
        key,
        ai_attempt,
        receipt_digest,
    } = decode_enrolled(&json)?
    else {
        return Err(BotError::Storage);
    };
    let attempt = InferenceAttemptId::parse(&ai_attempt.to_string()).ok_or(BotError::Storage)?;
    let exact: bool = conn
        .query_row(
            "SELECT wait_id=?1 AND run_id=?2 AND ai_attempt_id=?3 AND wait_revision=?4 AND origin_attempt_id=?5
             AND origin_generation=?6 AND origin_session_id=?7 AND origin_turn_id=?8 AND receipt_digest=?9
             FROM bot_child_wait_enrollments WHERE wait_id=?10",
            params![
                key.wait_id.to_string(),
                key.run.to_storage(),
                attempt.to_storage(),
                key.wait_revision.get(),
                key.origin_attempt.to_storage(),
                key.origin_generation.get(),
                key.origin_session.to_string(),
                key.origin_turn.to_string(),
                receipt_digest.as_bytes().as_slice(),
                wait.to_string()
            ],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if !exact || key.wait_id != wait {
        return Err(BotError::Storage);
    }
    Ok(Some((key, attempt)))
}

fn enrolled(
    key: BotChildWaitEnrollmentKey,
    attempt: InferenceAttemptId,
) -> BotChildWaitEnrollmentReply {
    BotChildWaitEnrollmentReply::Enrolled {
        receipt_digest: key.receipt_digest,
        ai_attempt: attempt.as_uuid(),
        key,
    }
}

/// Insert if absent, else require the same receipt digest. The caller's
/// transaction; nothing is committed here.
pub(super) fn enroll_on(
    tx: &Connection,
    owner: BotOwner,
    intent: &BotChildWaitCleanupIntentV1,
    now: Timestamp,
) -> Result<BotChildWaitEnrollmentReply, BotError> {
    intent.validate().map_err(|_| BotError::Invalid)?;
    let key = intent.key().map_err(|_| BotError::Invalid)?;
    validate_owner(owner, &key)?;
    if let Some((stored, attempt)) = retained_on(tx, key.wait_id)? {
        // The digest covers every field of the frozen intent. The wait's
        // current revision is deliberately not part of this comparison.
        return if stored.receipt_digest == key.receipt_digest && stored == key {
            Ok(enrolled(stored, attempt))
        } else {
            Err(BotError::Conflict)
        };
    }
    let (attempt, retained, replay) = subject_on(tx, owner, key.bot, key.run)?;
    let current = replay.fence.as_ref().ok_or(BotError::Storage)?;
    let origin = origin_fence(&key.origin());
    if *current != origin {
        return Err(BotError::Conflict);
    }
    let original = match &retained.outcome {
        Some(BotExecutionOutcome::Accepted { admission, .. })
            if matches!(
                retained.stage,
                BotReceiptStage::Accepted | BotReceiptStage::Completed
            ) =>
        {
            admission.fence.as_ref().ok_or(BotError::Storage)?
        }
        _ => return Err(BotError::Conflict),
    };
    // The origin is the admission's own execution, or the consumer an earlier
    // wait of this Run installed: a decision's or a child wait's.
    if *original != origin && !decisions::installed_consumer_on(tx, attempt, &origin)? {
        return Err(BotError::Conflict);
    }
    if replay.stop_reason.is_some() || !attempt_live_on(tx, attempt)? {
        return Err(BotError::Conflict);
    }
    let reply = enrolled(key.clone(), attempt);
    reply.validate().map_err(bad)?;
    let json =
        String::from_utf8(encode_decision_value(&reply, DECISION_EVIDENCE_BYTES).map_err(bad)?)
            .map_err(bad)?;
    tx.execute(
        "INSERT INTO bot_child_wait_enrollments(wait_id,run_id,ai_attempt_id,wait_revision,origin_attempt_id,origin_generation,origin_session_id,origin_turn_id,receipt_digest,receipt_json,created_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            key.wait_id.to_string(),
            key.run.to_storage(),
            attempt.to_storage(),
            key.wait_revision.get(),
            key.origin_attempt.to_storage(),
            key.origin_generation.get(),
            key.origin_session.to_string(),
            key.origin_turn.to_string(),
            key.receipt_digest.as_bytes().as_slice(),
            json,
            now.millis()
        ],
    )
    .map_err(bad)?;
    Ok(reply)
}

impl Repository {
    /// The delegate's enroll route and the worker's own write. `request` must
    /// come from the caller's authenticated delegate reply or call: its shape
    /// is neither a signature nor authority. Answers the retained enrollment
    /// when there is one with the same digest; a different digest for the
    /// same wait is `request_conflict`.
    pub fn enroll_bot_child_wait(
        &mut self,
        owner: BotOwner,
        request: &EnrollBotChildWaitCleanup,
        now: Timestamp,
    ) -> Result<BotChildWaitEnrollmentReply, BotError> {
        validate_owner(owner, &request.key)?;
        request.validate().map_err(|_| BotError::Invalid)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let reply = enroll_on(&tx, owner, &request.cleanup_intent, now)?;
        tx.commit().map_err(bad)?;
        Ok(reply)
    }

    /// Historical lookup: the retained enrollment, or `Missing` once the
    /// subject is proven and the named origin is still the Run's current
    /// execution. Absence is never success before that proof.
    pub fn read_bot_child_wait_cleanup(
        &self,
        owner: BotOwner,
        request: &ReadBotChildWaitCleanup,
    ) -> Result<BotChildWaitEnrollmentReply, BotError> {
        validate_owner(owner, &request.key)?;
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        let prior = retained_on(&tx, request.key.wait_id)?;
        let (attempt, _, replay) = subject_on(&tx, owner, request.key.bot, request.key.run)?;
        if let Some((stored, enrolled_attempt)) = prior {
            if enrolled_attempt != attempt {
                return Err(BotError::Storage);
            }
            if stored != request.key {
                return Err(BotError::Conflict);
            }
            return Ok(enrolled(stored, attempt));
        }
        if replay.fence.as_ref() != Some(&origin_fence(&request.key.origin())) {
            return Err(BotError::Conflict);
        }
        Ok(BotChildWaitEnrollmentReply::Missing {
            key: request.key.clone(),
        })
    }

    /// The origin terminal interceptor's lookup: the one retained child-wait
    /// enrollment of this AI attempt on this *exact* origin execution fence,
    /// or `None` when that origin never enrolled one. A consumer, a later
    /// turn or a fence without a turn never matches. The answer is permission
    /// to ask the delegate whether the wait's stop applies, never proof of it.
    pub fn bot_child_wait_enrollment_for_origin(
        &self,
        owner: BotOwner,
        attempt: InferenceAttemptId,
        origin: &BotExecutionFence,
    ) -> Result<Option<BotChildWaitEnrollmentKey>, BotError> {
        if owner.user_scope != origin.owner.user_scope || owner.device != origin.owner.device {
            return Err(BotError::Forbidden);
        }
        if origin.generation.get() == 0 {
            return Err(BotError::Invalid);
        }
        let Some(turn) = origin.turn else {
            return Ok(None);
        };
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        let (count, wait): (i64, Option<String>) = tx
            .query_row(
                "SELECT count(*),max(CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END)
                 FROM bot_child_wait_enrollments INDEXED BY bot_child_wait_enrollments_run
                 WHERE run_id=?1 AND ai_attempt_id=?2 AND origin_attempt_id=?3
                 AND origin_generation=?4 AND origin_session_id=?5 AND origin_turn_id=?6",
                params![
                    origin.run.to_storage(),
                    attempt.to_storage(),
                    origin.attempt.to_storage(),
                    origin.generation.get(),
                    origin.session.to_string(),
                    turn.to_string()
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(bad)?;
        if count == 0 {
            return Ok(None);
        }
        // One origin turn is held at most once: a Run has one open wait, and
        // a later wait's origin is the successor the earlier one installed.
        if count != 1 {
            return Err(BotError::Storage);
        }
        let wait: BotWaitId = wait.ok_or(BotError::Storage)?.parse().map_err(bad)?;
        let (key, enrolled_attempt) = retained_on(&tx, wait)?.ok_or(BotError::Storage)?;
        if enrolled_attempt != attempt || origin_fence(&key.origin()) != *origin {
            return Err(BotError::Storage);
        }
        validate_owner(owner, &key)?;
        let (subject, _, _) = subject_on(&tx, owner, key.bot, key.run)?;
        if subject != attempt {
            return Err(BotError::Storage);
        }
        Ok(Some(key))
    }
}
