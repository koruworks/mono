// SPDX-License-Identifier: MPL-2.0
//! Atomic replay identity rollover to a child wait's successor, qualified by
//! an authenticated retained acknowledgment. Mirrors the decision rollover,
//! keyed by the wait: a Run may wait more than once, and each wait that is
//! resumed installs its own successor.
use super::*;

/// Local retained result for retiring only the matching old transient
/// staging. Not a protocol message and not permission to launch a consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotChildReplayRollover {
    pub wait: BotWaitId,
    pub ai_attempt: InferenceAttemptId,
    pub resume: ChildResume,
    pub old_fence: BotExecutionFence,
    pub old_last_sequence: Option<u64>,
    pub consumer_fence: BotExecutionFence,
    pub created_at: Timestamp,
}

/// The successor's execution, from an acknowledged resume. A child wait is
/// never resumed in the stopped session, so the cursor always restarts.
fn consumer_fence(resume: &ChildResume) -> Result<BotExecutionFence, BotError> {
    resume.validate().map_err(|_| BotError::Invalid)?;
    let DecisionResumeState::InputAcknowledged { acknowledgment } = &resume.state else {
        return Err(BotError::Invalid);
    };
    let (attempt, generation, session, _) = resume.consumer_identity().ok_or(BotError::Invalid)?;
    Ok(BotExecutionFence {
        owner: resume.origin.owner,
        bot: resume.origin.bot,
        run: resume.origin.run,
        attempt,
        generation,
        session: session.ok_or(BotError::Invalid)?,
        turn: Some(acknowledgment.turn),
    })
}

const BOUNDED: &str = "typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36
 AND typeof(ai_attempt_id)='text' AND length(CAST(ai_attempt_id AS BLOB))=36
 AND typeof(old_attempt_id)='text' AND length(CAST(old_attempt_id AS BLOB))=36
 AND typeof(old_generation)='integer' AND old_generation BETWEEN 1 AND 4294967295
 AND typeof(old_session_id)='text' AND length(CAST(old_session_id AS BLOB))=36
 AND typeof(old_turn_id)='text' AND length(CAST(old_turn_id AS BLOB))=36
 AND (old_last_sequence IS NULL OR (typeof(old_last_sequence)='integer' AND old_last_sequence>=0))
 AND typeof(consumer_attempt_id)='text' AND length(CAST(consumer_attempt_id AS BLOB))=36
 AND typeof(consumer_generation)='integer' AND consumer_generation BETWEEN 1 AND 4294967295
 AND typeof(consumer_session_id)='text' AND length(CAST(consumer_session_id AS BLOB))=36
 AND typeof(consumer_turn_id)='text' AND length(CAST(consumer_turn_id AS BLOB))=36
 AND installed_after_sequence IS NULL
 AND typeof(resume_receipt_json)='text' AND length(CAST(resume_receipt_json AS BLOB)) BETWEEN 1 AND 65536
 AND typeof(created_at_ms)='integer'";

const SELECTOR: &str = "wait_id=?1 OR (ai_attempt_id=?2 AND consumer_attempt_id=?3 AND consumer_generation=?4 AND consumer_session_id=?5 AND consumer_turn_id=?6)";

/// The retained rollover of this wait or of this exact installed consumer.
fn archive_on(
    conn: &Connection,
    wait: BotWaitId,
    attempt: InferenceAttemptId,
    consumer: &BotExecutionFence,
) -> Result<Option<BotChildReplayRollover>, BotError> {
    let turn = consumer.turn.map(|v| v.to_string());
    let (count, bounded): (i64, Option<bool>) = conn
        .query_row(
            &format!(
                "SELECT count(*),min({BOUNDED}) FROM bot_child_replay_rollovers WHERE {SELECTOR}"
            ),
            params![
                wait.to_string(),
                attempt.to_storage(),
                consumer.attempt.to_storage(),
                consumer.generation.get(),
                consumer.session.to_string(),
                turn
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(bad)?;
    if count == 0 {
        return Ok(None);
    }
    if bounded != Some(true) {
        return Err(BotError::Storage);
    }
    // Each key is unique. Crosswiring two existing keys is a changed request.
    if count == 2 {
        return Err(BotError::Conflict);
    }
    if count != 1 {
        return Err(BotError::Storage);
    }
    let (body, old_cursor, at): (String, Option<u64>, i64) = conn
        .query_row(
            &format!("SELECT resume_receipt_json,old_last_sequence,created_at_ms FROM bot_child_replay_rollovers WHERE {SELECTOR}"),
            params![
                wait.to_string(),
                attempt.to_storage(),
                consumer.attempt.to_storage(),
                consumer.generation.get(),
                consumer.session.to_string(),
                turn
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(bad)?;
    let resume: ChildResume = serde_json::from_str(&body).map_err(bad)?;
    let old = origin_fence(&resume.origin);
    let installed = consumer_fence(&resume).map_err(|_| BotError::Storage)?;
    let exact: bool = conn
        .query_row(
            &format!("SELECT wait_id=?7 AND ai_attempt_id=?2 AND old_attempt_id=?8 AND old_generation=?9 AND old_session_id=?10 AND old_turn_id=?11
             AND consumer_attempt_id=?12 AND consumer_generation=?13 AND consumer_session_id=?14 AND consumer_turn_id=?15
             FROM bot_child_replay_rollovers WHERE {SELECTOR}"),
            params![
                wait.to_string(),
                attempt.to_storage(),
                consumer.attempt.to_storage(),
                consumer.generation.get(),
                consumer.session.to_string(),
                turn,
                resume.wait.to_string(),
                old.attempt.to_storage(),
                old.generation.get(),
                old.session.to_string(),
                old.turn.map(|v| v.to_string()),
                installed.attempt.to_storage(),
                installed.generation.get(),
                installed.session.to_string(),
                installed.turn.map(|v| v.to_string())
            ],
            |r| r.get(0),
        )
        .map_err(bad)?;
    if !exact {
        return Err(BotError::Storage);
    }
    Ok(Some(BotChildReplayRollover {
        wait: resume.wait,
        ai_attempt: attempt,
        resume,
        old_fence: old,
        old_last_sequence: old_cursor,
        consumer_fence: installed,
        created_at: Timestamp::from_millis(at),
    }))
}

/// The exact consumer a child wait installed for this AI attempt, or none.
/// Not a generation test or a history walk. The historical proof is tied to
/// its own retained enrollment; callers still own every fresh replay, policy
/// and Stop check.
pub(in crate::repository::bots) fn installed_child_consumer_on(
    conn: &Connection,
    attempt: InferenceAttemptId,
    fence: &BotExecutionFence,
) -> Result<Option<BotChildReplayRollover>, BotError> {
    let wait: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END
             FROM bot_child_replay_rollovers WHERE ai_attempt_id=?1 AND consumer_attempt_id=?2 AND consumer_generation=?3 AND consumer_session_id=?4 AND consumer_turn_id IS ?5",
            params![
                attempt.to_storage(),
                fence.attempt.to_storage(),
                fence.generation.get(),
                fence.session.to_string(),
                fence.turn.map(|v| v.to_string())
            ],
            |r| r.get(0),
        )
        .optional()
        .map_err(bad)?;
    let Some(wait) = wait else {
        return Ok(None);
    };
    let wait: BotWaitId = wait.parse().map_err(bad)?;
    let archive = archive_on(conn, wait, attempt, fence)?.ok_or(BotError::Storage)?;
    if archive.consumer_fence != *fence {
        return Err(BotError::Storage);
    }
    let (key, enrolled) = enrollment::retained_on(conn, wait)?.ok_or(BotError::Storage)?;
    if enrolled != attempt
        || key.owner != fence.owner
        || key.bot != fence.bot
        || key.run != fence.run
        || archive.old_fence != origin_fence(&key.origin())
    {
        return Err(BotError::Storage);
    }
    Ok(Some(archive))
}

impl Repository {
    /// Install the successor of a resumed child wait as the AI attempt's
    /// replay identity. `acknowledged` must be an independently authenticated
    /// retained delegate observation whose resume is `InputAcknowledged`: no
    /// DTO shape authenticates this argument. Exact replay answers the
    /// retained rollover; anything different for the same wait or the same
    /// consumer is `request_conflict`.
    pub fn rollover_bot_child_replay(
        &mut self,
        owner: BotOwner,
        enrollment: &BotChildWaitEnrollmentKey,
        acknowledged: &ChildResume,
        expected_old_fence: &BotExecutionFence,
        expected_old_cursor: Option<u64>,
        now: Timestamp,
    ) -> Result<BotChildReplayRollover, BotError> {
        if owner.user_scope != enrollment.owner.user_scope
            || owner.device != enrollment.owner.device
        {
            return Err(BotError::Forbidden);
        }
        enrollment.validate().map_err(|_| BotError::Invalid)?;
        let consumer = consumer_fence(acknowledged)?;
        if acknowledged.wait != enrollment.wait_id
            || acknowledged.origin != enrollment.origin()
            || *expected_old_fence != origin_fence(&acknowledged.origin)
            || expected_old_cursor.is_some_and(|v| v > i64::MAX as u64)
        {
            return Err(BotError::Conflict);
        }
        let encoded = String::from_utf8(encode_decision_value(acknowledged, 65536).map_err(bad)?)
            .map_err(bad)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let (stored, enrolled) =
            enrollment::retained_on(&tx, enrollment.wait_id)?.ok_or(BotError::NotFound)?;
        if stored != *enrollment {
            return Err(BotError::Conflict);
        }
        let (attempt, retained, replay) = subject_on(&tx, owner, enrollment.bot, enrollment.run)?;
        if enrolled != attempt {
            return Err(BotError::Storage);
        }
        if let Some(prior) = archive_on(&tx, acknowledged.wait, attempt, &consumer)? {
            if prior.resume != *acknowledged
                || prior.old_fence != *expected_old_fence
                || prior.old_last_sequence != expected_old_cursor
            {
                return Err(BotError::Conflict);
            }
            return Ok(prior);
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
        if original != expected_old_fence
            && !decisions::installed_consumer_on(&tx, attempt, expected_old_fence)?
        {
            return Err(BotError::Conflict);
        }
        if replay.fence.as_ref() != Some(expected_old_fence)
            || replay.last_sequence != expected_old_cursor
            || replay.stop_reason.is_some()
            || !attempt_live_on(&tx, attempt)?
        {
            return Err(BotError::Conflict);
        }
        tx.execute(
            "INSERT INTO bot_child_replay_rollovers(wait_id,ai_attempt_id,old_attempt_id,old_generation,old_session_id,old_turn_id,old_last_sequence,consumer_attempt_id,consumer_generation,consumer_session_id,consumer_turn_id,installed_after_sequence,resume_receipt_json,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,?12,?13)",
            params![
                acknowledged.wait.to_string(),
                attempt.to_storage(),
                expected_old_fence.attempt.to_storage(),
                expected_old_fence.generation.get(),
                expected_old_fence.session.to_string(),
                expected_old_fence.turn.map(|v| v.to_string()),
                expected_old_cursor,
                consumer.attempt.to_storage(),
                consumer.generation.get(),
                consumer.session.to_string(),
                consumer.turn.map(|v| v.to_string()),
                encoded,
                now.millis()
            ],
        )
        .map_err(bad)?;
        // A successor is a new session: its event cursor starts again.
        let changed = tx
            .execute(
                "UPDATE bot_run_replay SET run_attempt_id=?2,generation=?3,session_id=?4,turn_id=?5,last_sequence=NULL,updated_at_ms=?6
                 WHERE ai_attempt_id=?1 AND run_attempt_id=?7 AND generation=?8 AND session_id=?9 AND turn_id IS ?10 AND last_sequence IS ?11 AND stop_reason IS NULL",
                params![
                    attempt.to_storage(),
                    consumer.attempt.to_storage(),
                    consumer.generation.get(),
                    consumer.session.to_string(),
                    consumer.turn.map(|v| v.to_string()),
                    now.millis(),
                    expected_old_fence.attempt.to_storage(),
                    expected_old_fence.generation.get(),
                    expected_old_fence.session.to_string(),
                    expected_old_fence.turn.map(|v| v.to_string()),
                    expected_old_cursor
                ],
            )
            .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        let result = BotChildReplayRollover {
            wait: acknowledged.wait,
            ai_attempt: attempt,
            resume: acknowledged.clone(),
            old_fence: expected_old_fence.clone(),
            old_last_sequence: expected_old_cursor,
            consumer_fence: consumer,
            created_at: now,
        };
        tx.commit().map_err(bad)?;
        Ok(result)
    }
}
