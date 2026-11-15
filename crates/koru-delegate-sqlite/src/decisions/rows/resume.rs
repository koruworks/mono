// SPDX-License-Identifier: MPL-2.0

use super::*;
const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("decision_id", "text", 36, false),
    ("run_id", "text", 36, false),
    ("kind", "text", 32, false),
    ("origin_attempt_id", "text", 36, false),
    ("origin_generation", "integer", 0, false),
    ("origin_session_id", "text", 36, false),
    ("consumer_attempt_id", "text", 36, false),
    ("consumer_generation", "integer", 0, false),
    ("consumer_session_id", "text", 36, true),
    ("session_creation_key", "text", 36, false),
    ("input_key", "text", 36, false),
    ("input_fingerprint", "blob", 32, false),
    ("accepted_turn_id", "text", 36, true),
    ("state", "text", 32, false),
    ("receipt_json", "text", DECISION_RECEIPT_BYTES, true),
    ("created_at_ms", "integer", 0, false),
    ("updated_at_ms", "integer", 0, false),
];
pub(super) fn read(
    conn: &Connection,
    receipt: &DecisionReceipt,
) -> Result<Option<DecisionResume>, BotLedgerError> {
    conn.query_row(
        &query("bot_decision_resumes", FIELDS, "decision_id=?1"),
        [receipt.decision.to_storage()],
        |r| Ok(decode(conn, r, receipt)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

/// The acknowledged continuation is a separate immutable input owner. Resolve
/// its bounded scalar/acknowledgment row, never walk older decision bindings.
pub(in crate::decisions) fn accepted_input(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    run: RunId,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
    session: SessionId,
    turn: AgentTurnId,
) -> Result<bool, BotLedgerError> {
    let mut statement=conn.prepare(&query("bot_decision_resumes",FIELDS,
        "run_id=?1 AND consumer_attempt_id=?2 AND consumer_generation=?3 AND accepted_turn_id=?4 LIMIT 2")).map_err(sql)?;
    let mut cursor = statement
        .query(params![
            run.to_storage(),
            attempt.to_storage(),
            generation.get(),
            turn.to_string()
        ])
        .map_err(sql)?;
    let Some(row) = cursor.next().map_err(sql)? else {
        return Ok(false);
    };
    let c = Cells {
        row,
        table: "bot_decision_resumes",
    };
    c.check()?;
    let decision: DecisionId = c.id("decision_id")?;
    let acknowledgment: DecisionInputAcknowledgment = c.json("receipt_json")?;
    require(
        acknowledgment.turn == turn
            && c.optional_id::<AgentTurnId>("accepted_turn_id")? == Some(turn)
            && c.optional_id::<SessionId>("consumer_session_id")? == Some(session)
            && matches!(
                c.get::<String>("state")?.as_str(),
                "input_acknowledged" | "closed"
            ),
    )?;
    let creation: IdempotencyKey = c.id("session_creation_key")?;
    let origin_generation = c.generation("origin_generation")?;
    require(
        c.get::<String>("kind")? == "recorded_successor"
            && origin_generation.get().checked_add(1) == Some(generation.get())
            && c.id::<RunAttemptId>("origin_attempt_id")? != attempt
            && c.id::<SessionId>("origin_session_id")? != session,
    )?;
    let retained =
        crate::attempts::retained_bounded(conn, run, generation)?.ok_or(BotLedgerError::Fenced)?;
    require(
        retained.attempt == attempt
            && retained.provider_session
                == Some(ProviderSessionKey {
                    idempotency: creation,
                    session: Some(session),
                }),
    )?;
    let settled:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_decisions WHERE decision_id=?1 AND run_id=?2 AND user_scope=?3 AND device_id=?4 AND bot_id=?5 AND kind='koru_edit' AND phase='settled')",
        params![decision.to_storage(),run.to_storage(),owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage()],|r|r.get(0)).map_err(sql)?;
    require(cursor.next().map_err(sql)?.is_none())?;
    Ok(settled)
}
fn decode(
    conn: &Connection,
    row: &Row<'_>,
    receipt: &DecisionReceipt,
) -> Result<DecisionResume, BotLedgerError> {
    let c = Cells {
        row,
        table: "bot_decision_resumes",
    };
    c.check()?;
    let origin = receipt.binding.origin();
    c.same(
        receipt.state.kind == DecisionKind::KoruEdit
            && c.id::<DecisionId>("decision_id")? == receipt.decision
            && c.id::<RunId>("run_id")? == origin.run
            && c.id::<RunAttemptId>("origin_attempt_id")? == origin.attempt
            && c.generation("origin_generation")? == origin.generation
            && c.id::<SessionId>("origin_session_id")? == origin.session,
    )?;
    let attempt = c.id("consumer_attempt_id")?;
    let generation = c.generation("consumer_generation")?;
    let session = c.optional_id("consumer_session_id")?;
    let creation_key = c.id("session_creation_key")?;
    let consumer = match c.get::<String>("kind")?.as_str() {
        "same_session" => {
            c.same(
                attempt == origin.attempt
                    && generation == origin.generation
                    && session == Some(origin.session),
            )?;
            DecisionConsumer::SameSession { creation_key }
        }
        "recorded_successor" => DecisionConsumer::RecordedSuccessor {
            attempt,
            generation,
            session,
            creation_key,
        },
        _ => return Err(corrupt(c.table, "kind")),
    };
    let acknowledgment: Option<DecisionInputAcknowledgment> = c.optional_json("receipt_json")?;
    c.same(acknowledgment.as_ref().map(|a| a.turn) == c.optional_id("accepted_turn_id")?)?;
    let state = match (c.get::<String>("state")?.as_str(), acknowledgment) {
        ("prepared", None) => DecisionResumeState::Prepared {},
        ("session_bound", None) => DecisionResumeState::SessionBound {},
        ("input_unconfirmed", None) => DecisionResumeState::InputUnconfirmed {},
        ("input_acknowledged", Some(acknowledgment)) => {
            DecisionResumeState::InputAcknowledged { acknowledgment }
        }
        ("closed", acknowledgment) => DecisionResumeState::Closed { acknowledgment },
        _ => return Err(corrupt(c.table, "acknowledgment")),
    };
    let result = DecisionResume {
        decision: receipt.decision,
        origin,
        consumer,
        input_key: c.id("input_key")?,
        input_fingerprint: *c.digest("input_fingerprint")?.as_bytes(),
        state,
    };
    result.validate().map_err(|_| corrupt(c.table, "resume"))?;
    let answer = receipt
        .answer
        .as_ref()
        .ok_or_else(|| corrupt(c.table, "answer"))?;
    c.same(
        matches!(answer.choice, DecisionAnswerChoice::AllowOnce {})
            && c.get::<i64>("created_at_ms")? >= answer.answered_at_ms,
    )?;
    let retained = crate::attempts::retained_bounded(conn, origin.run, generation)?
        .ok_or_else(|| corrupt(c.table, "consumer_attempt"))?;
    c.same(
        retained.attempt == attempt
            && retained
                .provider_session
                .is_some_and(|s| s.idempotency == creation_key && s.session == session)
            && c.get::<i64>("updated_at_ms")? >= c.get::<i64>("created_at_ms")?,
    )?;
    Ok(result)
}
