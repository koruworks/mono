// SPDX-License-Identifier: MPL-2.0

use super::*;
const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("wait_id", "text", 36, false),
    ("run_id", "text", 36, false),
    ("decision_id", "text", 36, true),
    ("user_scope", "text", 36, false),
    ("device_id", "text", 36, false),
    ("bot_id", "text", 36, false),
    ("attempt_id", "text", 36, false),
    ("generation", "integer", 0, false),
    ("session_id", "text", 36, false),
    ("turn_id", "text", 36, false),
    ("observed_sequence", "integer", 0, true),
    ("reason", "text", 32, false),
    ("phase", "text", 32, false),
    ("revision", "integer", 0, false),
    (
        "ai_cleanup_receipt_json",
        "text",
        DECISION_EVIDENCE_BYTES,
        true,
    ),
    (
        "release_evidence_json",
        "text",
        DECISION_EVIDENCE_BYTES,
        true,
    ),
    (
        "cleanup_evidence_json",
        "text",
        DECISION_EVIDENCE_BYTES,
        true,
    ),
    (
        "ai_enrollment_request_json",
        "text",
        DECISION_EVIDENCE_BYTES,
        true,
    ),
    ("created_at_ms", "integer", 0, false),
    ("updated_at_ms", "integer", 0, false),
    ("settled_at_ms", "integer", 0, true),
];
pub(super) fn read_wait(
    conn: &Connection,
    run: RunId,
    id: DecisionId,
    receipt: &DecisionReceipt,
) -> Result<DecisionWaitRecord, BotLedgerError> {
    let record = conn
        .query_row(
            &query("bot_wait_runtime", FIELDS, "decision_id=?1"),
            [id.to_storage()],
            |r| Ok(decode(r)),
        )
        .optional()
        .map_err(sql)?
        .transpose()?
        .ok_or_else(|| corrupt("bot_wait_runtime", "missing"))?;
    require(
        record.origin.run == run
            && record.origin == receipt.binding.origin()
            && record.reason == BotWaitReason::Decision { decision: id },
    )?;
    Ok(record)
}
pub(in crate::decisions) fn by_wait(
    conn: &Connection,
    run: RunId,
    wait: BotWaitId,
) -> Result<Option<DecisionWaitRecord>, BotLedgerError> {
    conn.query_row(
        &query("bot_wait_runtime", FIELDS, "run_id=?1 AND wait_id=?2"),
        params![run.to_storage(), wait.to_storage()],
        |r| Ok(decode(r)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}
fn decode(row: &Row<'_>) -> Result<DecisionWaitRecord, BotLedgerError> {
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
    let reason = match (
        c.get::<String>("reason")?.as_str(),
        c.optional_id("decision_id")?,
    ) {
        ("decision", Some(decision)) => BotWaitReason::Decision { decision },
        ("capacity", None) => BotWaitReason::Capacity {},
        ("native_expired", None) => BotWaitReason::NativeExpired {},
        ("overlapping_prompt", None) => BotWaitReason::OverlappingPrompt {},
        _ => return Err(corrupt(c.table, "reason")),
    };
    let record = DecisionWaitRecord {
        wait: c.id("wait_id")?,
        origin,
        reason,
        phase: c.id("phase")?,
        revision: c.revision("revision")?,
        observed_sequence: c.get("observed_sequence")?,
        enrollment_request: c.optional_json("ai_enrollment_request_json")?,
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
            && (!matches!(record.phase, BotWaitPhase::Held | BotWaitPhase::Retired)
                || record.release.is_some()),
    )?;
    if let Some(request) = &record.enrollment_request {
        valid(request).map_err(|_| corrupt(c.table, "enrollment_request"))?;
        c.same(
            request.cleanup_intent.origin() == origin
                && request.key.wait_id == record.wait
                && record.reason
                    == BotWaitReason::Decision {
                        decision: request.key.decision_id,
                    }
                && request.key.wait_revision <= record.revision,
        )?;
    }
    if let Some(reply) = &record.enrollment {
        let request = record
            .enrollment_request
            .as_ref()
            .ok_or_else(|| corrupt(c.table, "enrollment_request"))?;
        c.same(
            matches!(reply, BotWaitEnrollmentReply::Enrolled { .. })
                && reply.matches_request(&request.key, false),
        )?;
    }
    Ok(record)
}

pub(super) fn correlate(
    conn: &Connection,
    receipt: &DecisionReceipt,
    wait: &DecisionWaitRecord,
    resume: Option<&DecisionResume>,
    ai_attempt: Option<uuid::Uuid>,
) -> Result<(), BotLedgerError> {
    let bad = || corrupt("bot_wait_runtime", "evidence");
    if let Some(BotWaitEnrollmentReply::Enrolled {
        ai_attempt: enrolled,
        ..
    }) = &wait.enrollment
    {
        if ai_attempt != Some(*enrolled) {
            return Err(corrupt("bot_wait_runtime", "enrollment_ai_attempt"));
        }
    }
    if let Some(request) = &wait.enrollment_request {
        if receipt.state.kind != DecisionKind::KoruEdit
            || request.key.decision_revision > receipt.state.revision
        {
            return Err(bad());
        }
    }
    if resume.is_some() && matches!(wait.phase, BotWaitPhase::Held | BotWaitPhase::Retired) {
        return Err(bad());
    }
    correlate_release(conn, wait)?;
    let original = authority::attempt(conn, &wait.origin)?;
    if let Some(cleanup) = &wait.cleanup {
        valid(cleanup).map_err(|_| bad())?;
        let ConsumerCleanup::Confirmed {
            target,
            creation_retirement,
            attempt_retirement,
            ..
        } = cleanup
        else {
            return Err(bad());
        };
        let expected = match resume {
            Some(r) => DecisionCleanupTarget::from_resume(r).map_err(|_| bad())?,
            None => DecisionCleanupTarget::Origin {
                fence: wait.origin,
                creation_key: original.provider_session.ok_or_else(bad)?.idempotency,
            },
        };
        if target != &expected {
            return Err(bad());
        }
        let (attempt, generation) = expected.attempt();
        let stored = crate::attempts::retained_bounded(conn, wait.origin.run, generation)?
            .ok_or_else(bad)?;
        if stored.attempt != attempt
            || !stored.retired
            || stored.host_instance != Some(creation_retirement.host)
            || stored.provider_session.is_none_or(|p| {
                p.idempotency != expected.creation_key() || p.session != expected.session()
            })
            || attempt_retirement.retired_at_ms > stored.updated_at_ms
        {
            return Err(bad());
        }
    }
    Ok(())
}

/// Exact retained origin release correlation, shared with the bounded capacity
/// projection. No complete decision payload or current execution grant is read.
pub(in crate::decisions) fn correlate_release(
    conn: &Connection,
    wait: &DecisionWaitRecord,
) -> Result<(), BotLedgerError> {
    let bad = || corrupt("bot_wait_runtime", "release_evidence");
    let original = authority::attempt(conn, &wait.origin)?;
    if let Some(release) = &wait.release {
        let OriginWaitStop::Confirmed {
            origin_fence,
            wait_id,
            intent_digest,
            enrollment_digest,
            creation_retirement,
            native_drain,
            ..
        } = release
        else {
            return Err(bad());
        };
        let request = wait.enrollment_request.as_ref().ok_or_else(bad)?;
        let target = DecisionCleanupTarget::Origin {
            fence: wait.origin,
            creation_key: original.provider_session.ok_or_else(bad)?.idempotency,
        };
        valid(native_drain).map_err(|_| bad())?;
        if *origin_fence != wait.origin
            || *wait_id != wait.wait
            || *intent_digest != request.key.receipt_digest
            || *enrollment_digest != request.key.receipt_digest
            || wait.enrollment.is_none()
            || !original.retired
            || !original.phase.is_terminal()
            || original.provider_session.and_then(|p|p.session)!=Some(wait.origin.session)
            || original.host_instance != Some(creation_retirement.host)
            || !creation_retirement.matches_target(&target)
            || native_drain.run != wait.origin.run
            || native_drain.attempt != wait.origin.attempt
            || native_drain.generation != wait.origin.generation
            || native_drain
                .effects
                .as_slice()
                .iter()
                .any(|e| e.binding.user_scope != wait.origin.owner.user_scope)
        {
            return Err(bad());
        }
        super::super::drain::correlate(conn, wait.origin.owner, wait.origin.bot, native_drain)?;
    }
    Ok(())
}
