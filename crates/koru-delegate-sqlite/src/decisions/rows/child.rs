// SPDX-License-Identifier: MPL-2.0

//! The child shapes of the shared wait and resume decoders (M6). A child
//! wait has no decision: its enrollment request is the frozen child intent,
//! its receipt the child reply, and its release an origin stop whose cause
//! is `child_wait_cleanup`. A row that mixes the two shapes is corrupt.
use super::*;
use crate::ChildWaitRecord;
use koru_delegate_core::children::{
    BotChildWaitCleanupIntentV1, BotChildWaitEnrollmentReply, ChildResume,
};

/// The Run's child wait with this id, through the same bounded projection
/// as every wait read. `None` when the row is absent or is not a child wait.
pub(crate) fn wait(
    conn: &Connection,
    run: RunId,
    wait: BotWaitId,
) -> Result<Option<ChildWaitRecord>, BotLedgerError> {
    conn.query_row(
        &query(
            "bot_wait_runtime",
            super::wait::FIELDS,
            "run_id=?1 AND wait_id=?2 AND reason='child'",
        ),
        params![run.to_storage(), wait.to_storage()],
        |r| Ok(decode(r)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

/// The Run's open child wait, if it has one. A Run has at most one open wait.
pub(crate) fn open_wait(
    conn: &Connection,
    run: RunId,
) -> Result<Option<ChildWaitRecord>, BotLedgerError> {
    conn.query_row(
        &query(
            "bot_wait_runtime",
            super::wait::FIELDS,
            "run_id=?1 AND phase<>'settled' AND reason='child'",
        ),
        [run.to_storage()],
        |r| Ok(decode(r)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

/// Validate the child columns of a wait row whose reason is `child`. The
/// generic record keeps no decision-typed enrollment for it.
pub(super) fn decode(row: &Row<'_>) -> Result<ChildWaitRecord, BotLedgerError> {
    let c = Cells {
        row,
        table: "bot_wait_runtime",
    };
    c.check()?;
    let origin = DecisionFence {
        owner: BotOwner {
            user_scope: c.id("user_scope")?,
            device: c.id("device_id")?,
        },
        bot: c.id("bot_id")?,
        run: c.id("run_id")?,
        attempt: c.id("attempt_id")?,
        generation: c.generation("generation")?,
        session: c.id("session_id")?,
        turn: c.id("turn_id")?,
    };
    origin.validate().map_err(|_| corrupt(c.table, "origin"))?;
    c.same(
        c.get::<String>("reason")? == "child"
            && c.optional_id::<DecisionId>("decision_id")?.is_none()
            && c.get::<Option<u64>>("observed_sequence")?.is_none(),
    )?;
    let intent: BotChildWaitCleanupIntentV1 = c.json("ai_enrollment_request_json")?;
    intent
        .validate()
        .map_err(|_| corrupt(c.table, "enrollment_request"))?;
    let record = ChildWaitRecord {
        wait: c.id("wait_id")?,
        origin,
        phase: c.id("phase")?,
        revision: c.revision("revision")?,
        intent,
        enrollment: c.optional_json("ai_cleanup_receipt_json")?,
        release: c.optional_json("release_evidence_json")?,
        cleanup: c.optional_json("cleanup_evidence_json")?,
        created_at_ms: c.get("created_at_ms")?,
        updated_at_ms: c.get("updated_at_ms")?,
        settled_at_ms: c.get("settled_at_ms")?,
    };
    c.same(
        (record.phase == BotWaitPhase::Settled) == record.settled_at_ms.is_some()
            && record.updated_at_ms >= record.created_at_ms
            && !matches!(record.phase, BotWaitPhase::Fenced | BotWaitPhase::Held)
            && (record.phase != BotWaitPhase::Retired || record.release.is_some())
            && record.intent.origin() == origin
            && record.intent.wait_id == record.wait
            && record.intent.wait_revision <= record.revision,
    )?;
    if let Some(reply) = &record.enrollment {
        let key = record
            .intent
            .key()
            .map_err(|_| corrupt(c.table, "enrollment_request"))?;
        c.same(
            matches!(reply, BotChildWaitEnrollmentReply::Enrolled { .. })
                && reply.matches_request(&key, false),
        )?;
    }
    if let Some(release) = &record.release {
        c.same(matches!(
            release,
            OriginWaitStop::Confirmed {
                cause: BotWaitCleanupCause::ChildWaitCleanup,
                ..
            }
        ))?;
    }
    if let Some(cleanup) = &record.cleanup {
        valid(cleanup).map_err(|_| corrupt(c.table, "cleanup_evidence"))?;
    }
    Ok(record)
}

/// Exact retained origin release correlation for a child wait.
pub(crate) fn correlate_release(
    conn: &Connection,
    wait: &ChildWaitRecord,
) -> Result<(), BotLedgerError> {
    let digest = wait
        .intent
        .digest()
        .map_err(|_| corrupt("bot_wait_runtime", "enrollment_request"))?;
    super::wait::correlate_release_parts(
        conn,
        &wait.origin,
        wait.wait,
        wait.release.as_ref(),
        Some(digest),
        wait.enrollment.is_some(),
        BotWaitCleanupCause::ChildWaitCleanup,
    )
}

const RESUME: &[(&str, &str, usize, bool)] = &[
    ("wait_id", "text", 36, false),
    ("run_id", "text", 36, false),
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

/// The resume row of a child wait: always a recorded successor.
pub(crate) fn resume(
    conn: &Connection,
    wait: &ChildWaitRecord,
) -> Result<Option<ChildResume>, BotLedgerError> {
    conn.query_row(
        &query("bot_child_resumes", RESUME, "wait_id=?1"),
        [wait.wait.to_storage()],
        |r| Ok(decode_resume(conn, r, wait)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

fn decode_resume(
    conn: &Connection,
    row: &Row<'_>,
    wait: &ChildWaitRecord,
) -> Result<ChildResume, BotLedgerError> {
    let c = Cells {
        row,
        table: "bot_child_resumes",
    };
    c.check()?;
    let origin = wait.origin;
    c.same(
        c.id::<BotWaitId>("wait_id")? == wait.wait
            && c.id::<RunId>("run_id")? == origin.run
            && c.id::<RunAttemptId>("origin_attempt_id")? == origin.attempt
            && c.generation("origin_generation")? == origin.generation
            && c.id::<SessionId>("origin_session_id")? == origin.session,
    )?;
    let attempt = c.id("consumer_attempt_id")?;
    let generation = c.generation("consumer_generation")?;
    let session = c.optional_id("consumer_session_id")?;
    let creation_key = c.id("session_creation_key")?;
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
    let result = ChildResume {
        wait: wait.wait,
        origin,
        consumer: DecisionConsumer::RecordedSuccessor {
            attempt,
            generation,
            session,
            creation_key,
        },
        input_key: c.id("input_key")?,
        input_fingerprint: *c.digest("input_fingerprint")?.as_bytes(),
        state,
    };
    result.validate().map_err(|_| corrupt(c.table, "resume"))?;
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

/// The child wait whose recorded successor is exactly this attempt, with its
/// resume. `None` when the attempt is not a child-wait successor.
pub(crate) fn consumer(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
) -> Result<Option<(ChildWaitRecord, ChildResume)>, BotLedgerError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN typeof(wait_id)='text' AND length(CAST(wait_id AS BLOB))=36 THEN wait_id END
             FROM bot_child_resumes WHERE run_id=?1 AND consumer_generation=?2",
            params![run.to_storage(), attempt.generation.get()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(id) = id else { return Ok(None) };
    let id = id
        .parse()
        .map_err(|_| corrupt("bot_child_resumes", "wait_id"))?;
    let record = wait(conn, run, id)?.ok_or_else(|| corrupt("bot_child_resumes", "wait_id"))?;
    let resume = resume(conn, &record)?.ok_or_else(|| corrupt("bot_child_resumes", "wait_id"))?;
    let (consumer, generation, session, creation) = resume
        .consumer_identity()
        .ok_or_else(|| corrupt("bot_child_resumes", "resume"))?;
    require(
        consumer == attempt.attempt
            && generation == attempt.generation
            && attempt.provider_session
                == Some(ProviderSessionKey {
                    idempotency: creation,
                    session,
                }),
    )?;
    Ok(Some((record, resume)))
}

/// Whether this exact turn is the acknowledged continuation of a child-wait
/// successor. The open-wait fence is the caller's; this is identity only.
pub(in crate::decisions) fn accepted_input(
    conn: &Connection,
    run: RunId,
    attempt: &crate::AttemptRow,
    session: SessionId,
    turn: AgentTurnId,
) -> Result<bool, BotLedgerError> {
    let Some((_, resume)) = consumer(conn, run, attempt)? else {
        return Ok(false);
    };
    Ok(resume.accepted_turn() == Some(turn)
        && resume.consumer_identity().and_then(|c| c.2) == Some(session))
}
