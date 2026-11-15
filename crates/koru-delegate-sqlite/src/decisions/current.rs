// SPDX-License-Identifier: MPL-2.0

//! Fresh native work is distinct from retained effect reconciliation.
use super::*;

/// A successor is admitted by its retained owning row, never by generation > 1.
/// This is identity-only; fresh activity uses the separate current wait gate.
pub(crate) fn retained_consumer_on(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
) -> Result<bool, BotLedgerError> {
    Ok(consumer_record(conn, run, attempt)?.is_some())
}

/// Delivery history may name the exact recorded successor only once its
/// continuation input was positively acknowledged. A newer generation, a bound
/// session or an unconfirmed input never qualifies; a later Stop that closed
/// the resume keeps the acknowledged history and does not erase it.
pub(crate) fn acknowledged_continuation_on(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
) -> Result<bool, BotLedgerError> {
    let Some(record) = consumer_record(conn, run, attempt)? else {
        return Ok(false);
    };
    let resume = record.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    Ok(matches!(
        resume.state,
        DecisionResumeState::InputAcknowledged { .. }
            | DecisionResumeState::Closed {
                acknowledgment: Some(_)
            }
    ))
}

fn consumer_record(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
) -> Result<Option<DecisionRecord>, BotLedgerError> {
    let row:Option<(String,String,String)>=conn.query_row("SELECT CASE WHEN typeof(d.decision_id)='text' AND length(CAST(d.decision_id AS BLOB))=36 THEN d.decision_id END, CASE WHEN typeof(d.user_scope)='text' AND length(CAST(d.user_scope AS BLOB))=36 THEN d.user_scope END,CASE WHEN typeof(d.device_id)='text' AND length(CAST(d.device_id AS BLOB))=36 THEN d.device_id END FROM bot_decision_resumes r JOIN bot_decisions d ON d.decision_id=r.decision_id WHERE r.run_id=?1 AND r.kind='recorded_successor' AND r.consumer_generation=?2",params![run.to_storage(),attempt.generation.get()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    let Some((id, user, device)) = row else {
        return Ok(None);
    };
    let owner: BotOwner =
        serde_json::from_value(serde_json::json!({"user_scope":user,"device":device}))
            .map_err(|_| BotLedgerError::Invalid)?;
    let record = rows::read(
        conn,
        owner,
        run,
        id.parse().map_err(|_| BotLedgerError::Invalid)?,
    )?
    .ok_or(BotLedgerError::Fenced)?;
    let resume = record.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    let (id, generation, session, creation) = resume.consumer_identity();
    require(
        id == attempt.attempt
            && generation == attempt.generation
            && attempt.provider_session
                == Some(ProviderSessionKey {
                    idempotency: creation,
                    session,
                }),
    )?;
    Ok(Some(record))
}

/// Starting an active interval is fresh work; stopping the accounting interval
/// remains possible after authority loss. The sole reserved consumer may run
/// its exact retry while its decision owns the ordinary-work fence.
pub(crate) fn fresh_activity_on(
    conn: &Connection,
    admitted: &crate::BotAdmissionRecord,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
    proof: Option<&FreshBotPolicyProof>,
    now: i64,
) -> Result<(), BotLedgerError> {
    let run = admitted.submission.request.run;
    let owner = admitted.submission.provenance.owner;
    let Some(wait) = observation::execution(conn, owner, run)? else {
        return Ok(());
    };
    let expected = wait
        .expected
        .decision
        .as_ref()
        .ok_or(BotLedgerError::Fenced)?;
    let record = rows::read(conn, owner, run, expected.decision)?.ok_or(BotLedgerError::Fenced)?;
    require(
        record.wait.phase == BotWaitPhase::ResumeReserved
            && record.wait.cleanup.is_none()
            && matches!(
                record.receipt.state.phase,
                DecisionPhase::ResumePrepared | DecisionPhase::AwaitRetry | DecisionPhase::Effect
            ),
    )?;
    let resume = record.resume.as_ref().ok_or(BotLedgerError::Fenced)?;
    let (consumer, current, _, _) = resume.consumer_identity();
    require(
        consumer == attempt
            && current == generation
            && !matches!(resume.state, DecisionResumeState::Closed { .. })
            && now >= record.receipt.last_observed_at_ms
            && now < record.receipt.deadline.deadline_at_ms,
    )?;
    authority::current_policy(
        conn,
        &record.receipt.binding,
        proof.ok_or(BotLedgerError::Fenced)?,
    )?;
    Ok(())
}

pub(crate) fn fresh_native_input_on(
    conn: &Connection,
    key: &NativeResultKey,
    admitted: &crate::BotAdmissionRecord,
    attempt: &crate::AttemptRow,
) -> Result<(), BotLedgerError> {
    let fenced:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_decisions WHERE run_id=?1 AND phase<>'settled') OR EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled')",
        [key.run.to_storage()],|r|r.get(0)).map_err(sql)?;
    if fenced {
        return Err(BotLedgerError::Fenced);
    }
    if admitted.initial_send.as_ref().is_some_and(|s| {
        s.attempt == key.attempt && s.generation == key.generation && s.turn == Some(key.turn)
    }) {
        return Ok(());
    }
    let p = &admitted.submission.provenance;
    let session = attempt
        .provider_session
        .and_then(|p| p.session)
        .ok_or(BotLedgerError::Fenced)?;
    require(rows::accepted_input(
        conn,
        p.owner,
        p.bot,
        key.run,
        key.attempt,
        key.generation,
        session,
        key.turn,
    )?)
}
