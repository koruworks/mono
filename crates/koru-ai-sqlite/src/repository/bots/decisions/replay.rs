// SPDX-License-Identifier: MPL-2.0
//! Atomic replay identity rollover, qualified by an authenticated retained ack.
use super::*;
use koru_ai_core::bots::replay::BotExecutionFence;

/// Local retained result for retiring only the matching old transient staging.
/// This is not a protocol message or permission to launch a consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotDecisionReplayRollover {
    pub decision: DecisionId,
    pub ai_attempt: InferenceAttemptId,
    pub resume: DecisionResume,
    pub old_fence: BotExecutionFence,
    pub old_last_sequence: Option<u64>,
    pub consumer_fence: BotExecutionFence,
    pub installed_after_sequence: Option<u64>,
    pub created_at: Timestamp,
}

fn origin_fence(resume: &DecisionResume) -> BotExecutionFence {
    let o = &resume.origin;
    BotExecutionFence {
        owner: o.owner,
        bot: o.bot,
        run: o.run,
        attempt: o.attempt,
        generation: o.generation,
        session: o.session,
        turn: Some(o.turn),
    }
}

fn consumer_fence(resume: &DecisionResume) -> Result<BotExecutionFence, BotError> {
    resume.validate().map_err(|_| BotError::Invalid)?;
    let DecisionResumeState::InputAcknowledged { acknowledgment } = &resume.state else {
        return Err(BotError::Invalid);
    };
    let (attempt, generation, session, _) = resume.consumer_identity();
    if matches!(resume.consumer, DecisionConsumer::SameSession { .. })
        && acknowledgment.turn == resume.origin.turn
    {
        return Err(BotError::Invalid);
    }
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

/// The card already exists. Prove its immutable foreign identities without
/// decoding UI text, mutable projection state or execution configuration.
fn invocation_on(
    conn: &Connection,
    key: &BotWaitEnrollmentKey,
    attempt: InferenceAttemptId,
) -> Result<(), BotError> {
    let valid:bool=conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(d.decision_id)='text' AND length(CAST(d.decision_id AS BLOB))=36
         AND typeof(d.run_id)='text' AND length(CAST(d.run_id AS BLOB))=36
         AND typeof(d.ai_attempt_id)='text' AND length(CAST(d.ai_attempt_id AS BLOB))=36
         AND typeof(d.chat_id)='text' AND length(CAST(d.chat_id AS BLOB))=36
         AND typeof(d.assistant_message_id)='text' AND length(CAST(d.assistant_message_id AS BLOB))=36
         AND typeof(d.invocation_id)='text' AND length(CAST(d.invocation_id AS BLOB))=36
         AND typeof(d.decision_revision)='integer' AND d.decision_revision>=?4
         AND typeof(d.event_id)='text' AND length(CAST(d.event_id AS BLOB))=36
         AND typeof(d.created_at_ms)='integer' AND typeof(d.updated_at_ms)='integer'
         AND d.run_id=?2 AND d.ai_attempt_id=?3),0)
         FROM bot_decision_invocations d JOIN bot_chat_runs l ON l.ai_attempt_id=d.ai_attempt_id
          AND l.run_id=d.run_id AND l.chat_id=d.chat_id AND l.assistant_message_id=d.assistant_message_id
         JOIN tool_invocations t ON t.id=d.invocation_id AND t.attempt_id=d.ai_attempt_id AND t.chat_id=d.chat_id
         WHERE d.decision_id=?1",params![key.decision_id.to_storage(),key.run.to_storage(),attempt.to_storage(),key.decision_revision.get()],|r|r.get(0)).map_err(bad)?;
    if !valid {
        return Err(BotError::Storage);
    }
    let (event, invocation): (String, String) = conn
        .query_row(
            "SELECT event_id,invocation_id FROM bot_decision_invocations WHERE decision_id=?1",
            [key.decision_id.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(bad)?;
    event.parse::<koru_delegate_core::EventId>().map_err(bad)?;
    ToolInvocationId::parse(&invocation).ok_or(BotError::Storage)?;
    Ok(())
}

fn archive_on(
    conn: &Connection,
    decision: DecisionId,
    attempt: InferenceAttemptId,
    consumer: &BotExecutionFence,
) -> Result<Option<BotDecisionReplayRollover>, BotError> {
    let (count,bounded):(i64,Option<bool>)=conn.query_row(
        "SELECT count(*),min(typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36
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
         AND (installed_after_sequence IS NULL OR (typeof(installed_after_sequence)='integer' AND installed_after_sequence>=0))
         AND typeof(resume_receipt_json)='text' AND length(CAST(resume_receipt_json AS BLOB)) BETWEEN 1 AND 65536
         AND typeof(created_at_ms)='integer') FROM bot_decision_replay_rollovers
         WHERE decision_id=?1 OR (ai_attempt_id=?2 AND consumer_attempt_id=?3 AND consumer_generation=?4 AND consumer_session_id=?5 AND consumer_turn_id=?6)",
        params![decision.to_storage(),attempt.to_storage(),consumer.attempt.to_storage(),consumer.generation.get(),consumer.session.to_string(),consumer.turn.map(|v|v.to_string())],|r|Ok((r.get(0)?,r.get(1)?))).map_err(bad)?;
    if count == 0 {
        return Ok(None);
    }
    if bounded != Some(true) {
        return Err(BotError::Storage);
    }
    if count == 2 {
        return Err(BotError::Conflict);
    }
    if count != 1 {
        return Err(BotError::Storage);
    }
    let (body,old_cursor,new_cursor,at):(String,Option<u64>,Option<u64>,i64)=conn.query_row(
        "SELECT resume_receipt_json,old_last_sequence,installed_after_sequence,created_at_ms FROM bot_decision_replay_rollovers
         WHERE decision_id=?1 OR (ai_attempt_id=?2 AND consumer_attempt_id=?3 AND consumer_generation=?4 AND consumer_session_id=?5 AND consumer_turn_id=?6)",
        params![decision.to_storage(),attempt.to_storage(),consumer.attempt.to_storage(),consumer.generation.get(),consumer.session.to_string(),consumer.turn.map(|v|v.to_string())],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(bad)?;
    let resume: DecisionResume = serde_json::from_str(&body).map_err(bad)?;
    let old = origin_fence(&resume);
    let installed = consumer_fence(&resume).map_err(|_| BotError::Storage)?;
    let expected_cursor = if matches!(resume.consumer, DecisionConsumer::SameSession { .. }) {
        old_cursor
    } else {
        None
    };
    if new_cursor != expected_cursor {
        return Err(BotError::Storage);
    }
    let exact:bool=conn.query_row(
        "SELECT decision_id=?1 AND ai_attempt_id=?2 AND old_attempt_id=?3 AND old_generation=?4 AND old_session_id=?5 AND old_turn_id=?6
         AND consumer_attempt_id=?7 AND consumer_generation=?8 AND consumer_session_id=?9 AND consumer_turn_id=?10
         FROM bot_decision_replay_rollovers WHERE decision_id=?11 OR (ai_attempt_id=?12 AND consumer_attempt_id=?13 AND consumer_generation=?14 AND consumer_session_id=?15 AND consumer_turn_id=?16)",
        params![resume.decision.to_storage(),attempt.to_storage(),old.attempt.to_storage(),old.generation.get(),old.session.to_string(),old.turn.map(|v|v.to_string()),
            installed.attempt.to_storage(),installed.generation.get(),installed.session.to_string(),installed.turn.map(|v|v.to_string()),
            decision.to_storage(),attempt.to_storage(),consumer.attempt.to_storage(),consumer.generation.get(),consumer.session.to_string(),consumer.turn.map(|v|v.to_string())],|r|r.get(0)).map_err(bad)?;
    if !exact {
        return Err(BotError::Storage);
    }
    Ok(Some(BotDecisionReplayRollover {
        decision: resume.decision,
        ai_attempt: attempt,
        resume,
        old_fence: old,
        old_last_sequence: old_cursor,
        consumer_fence: installed,
        installed_after_sequence: new_cursor,
        created_at: Timestamp::from_millis(at),
    }))
}

/// Whether `fence` is the consumer some wait of this AI attempt installed: a
/// decision's or a child wait's (M6). A child rollover may follow a decision
/// rollover and the reverse, so both tables are read. Exact, not a generation
/// test or a history walk; callers still own all fresh current replay, policy
/// and Stop checks.
pub(in crate::repository::bots) fn installed_consumer_on(
    conn: &Connection,
    attempt: InferenceAttemptId,
    fence: &BotExecutionFence,
) -> Result<bool, BotError> {
    Ok(
        installed_decision_consumer_on(conn, attempt, fence)?.is_some()
            || super::super::children::rollover::installed_child_consumer_on(conn, attempt, fence)?
                .is_some(),
    )
}

/// Exact immediate consumer a decision installed, not a generation test or
/// history walk. Historical proof is correlated with its own retained
/// enrollment/invocation; callers still own all fresh current replay, policy
/// and Stop checks.
pub(super) fn installed_decision_consumer_on(
    conn: &Connection,
    attempt: InferenceAttemptId,
    fence: &BotExecutionFence,
) -> Result<Option<BotDecisionReplayRollover>, BotError> {
    let decision:Option<String>=conn.query_row(
        "SELECT CASE WHEN typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36 THEN decision_id END
         FROM bot_decision_replay_rollovers WHERE ai_attempt_id=?1 AND consumer_attempt_id=?2 AND consumer_generation=?3 AND consumer_session_id=?4 AND consumer_turn_id IS ?5",
        params![attempt.to_storage(),fence.attempt.to_storage(),fence.generation.get(),fence.session.to_string(),fence.turn.map(|v|v.to_string())],|r|r.get(0)).optional().map_err(bad)?;
    let Some(decision) = decision else {
        return Ok(None);
    };
    let decision = decision.parse().map_err(bad)?;
    let archive = archive_on(conn, decision, attempt, fence)?.ok_or(BotError::Storage)?;
    if archive.consumer_fence != *fence {
        return Err(BotError::Storage);
    }
    let receipt = enrollment::for_decision_on(conn, decision)?.ok_or(BotError::Storage)?;
    let BotWaitEnrollmentReply::Enrolled {
        key, ai_attempt, ..
    } = receipt
    else {
        return Err(BotError::Storage);
    };
    if ai_attempt != attempt.as_uuid()
        || key.owner != fence.owner
        || key.bot != fence.bot
        || key.run != fence.run
        || archive.old_fence.owner != key.owner
        || archive.old_fence.bot != key.bot
        || archive.old_fence.run != key.run
        || archive.old_fence.attempt != key.origin_attempt
        || archive.old_fence.generation != key.origin_generation
        || archive.old_fence.session != key.origin_session
        || archive.old_fence.turn != Some(key.origin_turn)
    {
        return Err(BotError::Storage);
    }
    invocation_on(conn, &key, attempt)?;
    Ok(Some(archive))
}

impl Repository {
    /// `acknowledged` must be an independently authenticated retained delegate
    /// InputAcknowledged observation. No DTO shape authenticates this argument.
    /// SameSession support here does not qualify the production release path.
    pub fn rollover_bot_decision_replay(
        &mut self,
        owner: BotOwner,
        enrollment: &BotWaitEnrollmentKey,
        acknowledged: &DecisionResume,
        expected_old_fence: &BotExecutionFence,
        expected_old_cursor: Option<u64>,
        now: Timestamp,
    ) -> Result<BotDecisionReplayRollover, BotError> {
        enrollment::validate_owner(owner, enrollment)?;
        let consumer = consumer_fence(acknowledged)?;
        if acknowledged.decision != enrollment.decision_id
            || acknowledged.origin
                != (DecisionFence {
                    owner: enrollment.owner,
                    bot: enrollment.bot,
                    run: enrollment.run,
                    attempt: enrollment.origin_attempt,
                    generation: enrollment.origin_generation,
                    session: enrollment.origin_session,
                    turn: enrollment.origin_turn,
                })
            || *expected_old_fence != origin_fence(acknowledged)
            || expected_old_cursor.is_some_and(|v| v > i64::MAX as u64)
        {
            return Err(BotError::Conflict);
        }
        let encoded = String::from_utf8(encode_decision_value(acknowledged, 65536).map_err(bad)?)
            .map_err(bad)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let enrolled = enrollment::retained_on(&tx, enrollment)?.ok_or(BotError::NotFound)?;
        let (attempt, retained, replay) = enrollment::subject_on(&tx, owner, enrollment)?;
        if !matches!(enrolled,BotWaitEnrollmentReply::Enrolled {ai_attempt,..} if ai_attempt==attempt.as_uuid())
        {
            return Err(BotError::Storage);
        }
        invocation_on(&tx, enrollment, attempt)?;
        if let Some(prior) = archive_on(&tx, acknowledged.decision, attempt, &consumer)? {
            if prior.resume != *acknowledged
                || prior.old_fence != *expected_old_fence
                || prior.old_last_sequence != expected_old_cursor
            {
                return Err(BotError::Conflict);
            }
            return Ok(prior);
        }
        let original_fence = match &retained.outcome {
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
        if original_fence != expected_old_fence
            && !installed_consumer_on(&tx, attempt, expected_old_fence)?
        {
            return Err(BotError::Conflict);
        }
        if replay.fence.as_ref() != Some(expected_old_fence)
            || replay.last_sequence != expected_old_cursor
            || replay.stop_reason.is_some()
        {
            return Err(BotError::Conflict);
        }
        let live:bool=tx.query_row("SELECT status IN ('pending','streaming') AND stop_reason IS NULL FROM inference_attempts WHERE id=?1",[attempt.to_storage()],|r|r.get(0)).map_err(bad)?;
        if !live {
            return Err(BotError::Conflict);
        }
        let installed_cursor =
            if matches!(acknowledged.consumer, DecisionConsumer::SameSession { .. }) {
                expected_old_cursor
            } else {
                None
            };
        tx.execute("INSERT INTO bot_decision_replay_rollovers(decision_id,ai_attempt_id,old_attempt_id,old_generation,old_session_id,old_turn_id,old_last_sequence,consumer_attempt_id,consumer_generation,consumer_session_id,consumer_turn_id,installed_after_sequence,resume_receipt_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![acknowledged.decision.to_storage(),attempt.to_storage(),expected_old_fence.attempt.to_storage(),expected_old_fence.generation.get(),expected_old_fence.session.to_string(),expected_old_fence.turn.map(|v|v.to_string()),expected_old_cursor,
                consumer.attempt.to_storage(),consumer.generation.get(),consumer.session.to_string(),consumer.turn.map(|v|v.to_string()),installed_cursor,encoded,now.millis()]).map_err(bad)?;
        let changed=tx.execute("UPDATE bot_run_replay SET run_attempt_id=?2,generation=?3,session_id=?4,turn_id=?5,last_sequence=?6,updated_at_ms=?7
            WHERE ai_attempt_id=?1 AND run_attempt_id=?8 AND generation=?9 AND session_id=?10 AND turn_id IS ?11 AND last_sequence IS ?12 AND stop_reason IS NULL",
            params![attempt.to_storage(),consumer.attempt.to_storage(),consumer.generation.get(),consumer.session.to_string(),consumer.turn.map(|v|v.to_string()),installed_cursor,now.millis(),
                expected_old_fence.attempt.to_storage(),expected_old_fence.generation.get(),expected_old_fence.session.to_string(),expected_old_fence.turn.map(|v|v.to_string()),expected_old_cursor]).map_err(bad)?;
        if changed != 1 {
            return Err(BotError::Conflict);
        }
        let result = BotDecisionReplayRollover {
            decision: acknowledged.decision,
            ai_attempt: attempt,
            resume: acknowledged.clone(),
            old_fence: expected_old_fence.clone(),
            old_last_sequence: expected_old_cursor,
            consumer_fence: consumer,
            installed_after_sequence: installed_cursor,
            created_at: now,
        };
        tx.commit().map_err(bad)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
