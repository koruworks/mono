// SPDX-License-Identifier: MPL-2.0
//! A decision's card: the one existing shared approval row the person
//! answers, linked to the decision's own identity (`bot_decision_invocations`).
//!
//! Exactly one card per decision. A Koru decision gets a new approval row,
//! written only for an enrolled wait (the AI's authenticated knowledge of that
//! decision) and never as part of the enrollment receipt, which stays
//! independent of any card. A native decision links the native prompt's own
//! approval row, which keeps its offered choices; a native id is never parsed
//! as a DecisionId. A card grants nothing: the delegate's ledger owns every
//! answer, revision and authority check.
use super::*;
use koru_ai_core::bots::replay::BotToolSourceKind;

/// The bounded retained link of one card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotDecisionCard {
    pub decision: DecisionId,
    pub run: koru_delegate_core::RunId,
    pub ai_attempt: InferenceAttemptId,
    pub invocation: ToolInvocationId,
    /// The decision revision the card was written or last advanced at.
    pub revision: DecisionRevision,
}

/// A native decision's identity, from its retained receipt. The prompt is the
/// native `ApprovalId` the existing approval row is already bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotNativeDecisionLink {
    pub bot: koru_delegate_core::BotId,
    pub run: koru_delegate_core::RunId,
    pub decision: DecisionId,
    pub revision: DecisionRevision,
    pub prompt: koru_delegate_core::ApprovalId,
}

/// One bounded read by the unique decision or invocation key.
fn card_on(
    conn: &Connection,
    column: &'static str,
    value: &str,
) -> Result<Option<BotDecisionCard>, BotError> {
    debug_assert!(matches!(column, "decision_id" | "invocation_id"));
    let sql = format!(
        "SELECT CASE WHEN typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36
          AND typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36
          AND typeof(ai_attempt_id)='text' AND length(CAST(ai_attempt_id AS BLOB))=36
          AND typeof(invocation_id)='text' AND length(CAST(invocation_id AS BLOB))=36
          AND typeof(decision_revision)='integer' AND decision_revision>0
         THEN decision_id||' '||run_id||' '||ai_attempt_id||' '||invocation_id END, decision_revision
         FROM bot_decision_invocations WHERE {column}=?1"
    );
    let row: Option<(Option<String>, i64)> = conn
        .query_row(&sql, [value], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(bad)?;
    let Some((ids, revision)) = row else {
        return Ok(None);
    };
    let ids = ids.ok_or(BotError::Storage)?;
    let mut parts = ids.split(' ');
    let (Some(decision), Some(run), Some(attempt), Some(invocation), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(BotError::Storage);
    };
    Ok(Some(BotDecisionCard {
        decision: decision.parse().map_err(bad)?,
        run: run.parse().map_err(bad)?,
        ai_attempt: InferenceAttemptId::parse(attempt).ok_or(BotError::Storage)?,
        invocation: ToolInvocationId::parse(invocation).ok_or(BotError::Storage)?,
        revision: DecisionRevision::new(u64::try_from(revision).map_err(bad)?).map_err(bad)?,
    }))
}

/// The retained attempt of a Bot Run, through the sole admission decoder.
fn attempt_for_run_on(
    conn: &Connection,
    owner: BotOwner,
    bot: koru_delegate_core::BotId,
    run: koru_delegate_core::RunId,
) -> Result<(InferenceAttemptId, String, String), BotError> {
    let (receipt, _) = routines::model::retained_run_subject_on(conn, owner, bot, run)?;
    let p = &receipt.intent.submission.provenance;
    let attempt = InferenceAttemptId::parse(&p.ai_attempt_id.ok_or(BotError::Storage)?.to_string())
        .ok_or(BotError::Storage)?;
    let link: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT CASE WHEN typeof(chat_id)='text' AND length(CAST(chat_id AS BLOB))=36 THEN chat_id END,
             CASE WHEN typeof(assistant_message_id)='text' AND length(CAST(assistant_message_id AS BLOB))=36 THEN assistant_message_id END
             FROM bot_chat_runs WHERE ai_attempt_id=?1 AND run_id=?2",
            params![attempt.to_storage(), run.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(bad)?;
    match link {
        Some((Some(chat), Some(message))) => Ok((attempt, chat, message)),
        _ => Err(BotError::Storage),
    }
}

fn link_on(
    tx: &Connection,
    decision: DecisionId,
    run: koru_delegate_core::RunId,
    attempt: InferenceAttemptId,
    chat: &str,
    message: &str,
    invocation: ToolInvocationId,
    revision: DecisionRevision,
    now: Timestamp,
) -> Result<(), BotError> {
    tx.execute(
        "INSERT INTO bot_decision_invocations(decision_id,run_id,ai_attempt_id,chat_id,assistant_message_id,invocation_id,decision_revision,event_id,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        params![
            decision.to_storage(),
            run.to_storage(),
            attempt.to_storage(),
            chat,
            message,
            invocation.to_storage(),
            decision_revision(revision)?,
            uuid::Uuid::new_v4().to_string(),
            now.millis()
        ],
    )
    .map_err(bad)?;
    Ok(())
}

fn decision_revision(revision: DecisionRevision) -> Result<i64, BotError> {
    i64::try_from(revision.get()).map_err(bad)
}

/// Exact replay: the existing card for this decision must be the same Run and
/// attempt. A card is never moved to another task or rewritten.
fn replay(
    existing: BotDecisionCard,
    run: koru_delegate_core::RunId,
    attempt: InferenceAttemptId,
) -> Result<BotDecisionCard, BotError> {
    if existing.run != run || existing.ai_attempt != attempt {
        return Err(BotError::Conflict);
    }
    Ok(existing)
}

impl Repository {
    /// The card answered for `invocation`, if that row is a decision's card.
    pub fn bot_decision_card(
        &self,
        invocation: ToolInvocationId,
    ) -> Result<Option<BotDecisionCard>, BotError> {
        card_on(&self.connection, "invocation_id", &invocation.to_storage())
    }

    /// Write the Koru decision's card exactly once, no later than its
    /// enrollment (O129, the rollover requires it). `card` is the new shared
    /// approval row as the AI owner built it (agent approval kind, Proposed);
    /// it is stored as an open request awaiting the person's answer.
    pub fn record_bot_decision_card(
        &mut self,
        owner: BotOwner,
        key: &BotWaitEnrollmentKey,
        card: &ToolActivity,
        now: Timestamp,
    ) -> Result<BotDecisionCard, BotError> {
        enrollment::validate_owner(owner, key)?;
        let tx = self.connection.transaction().map_err(bad)?;
        let enrolled = enrollment::retained_on(&tx, key)?.ok_or(BotError::NotFound)?;
        let (attempt, chat, message) = attempt_for_run_on(&tx, owner, key.bot, key.run)?;
        if !matches!(enrolled, BotWaitEnrollmentReply::Enrolled { ai_attempt, .. } if ai_attempt == attempt.as_uuid())
        {
            return Err(BotError::Storage);
        }
        if let Some(existing) = card_on(&tx, "decision_id", &key.decision_id.to_storage())? {
            return replay(existing, key.run, attempt);
        }
        if card.attempt_id != attempt
            || card.chat_id.to_storage() != chat
            || card.assistant_message_id.to_storage() != message
            || card.tool_name != koru_ai_core::AgentToolKind::Approval.as_token()
            || card.status != ToolActivityStatus::Proposed
            || card.approval != ToolApproval::Pending
            || card.live.is_some()
            || card.display_target.is_some()
        {
            return Err(BotError::Invalid);
        }
        let live: bool = tx
            .query_row(
                "SELECT status IN ('pending','streaming') FROM inference_attempts WHERE id=?1",
                [attempt.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if !live {
            return Err(BotError::Conflict);
        }
        crate::repository::mutations::create_tool_activity_on(&tx, card)
            .map_err(|_| BotError::Storage)?;
        let mut open = card.clone();
        open.status = ToolActivityStatus::ApprovalRequired;
        crate::repository::mutations::advance_agent_activity_on(&tx, &open)
            .map_err(|_| BotError::Storage)?;
        link_on(
            &tx,
            key.decision_id,
            key.run,
            attempt,
            &chat,
            &message,
            card.id,
            key.decision_revision,
            now,
        )?;
        let written =
            card_on(&tx, "decision_id", &key.decision_id.to_storage())?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(written)
    }

    /// Link a native decision to the native prompt's own open approval row.
    /// The row keeps its offered choices; only its decision identity is added.
    pub fn link_bot_native_decision_card(
        &mut self,
        owner: BotOwner,
        link: &BotNativeDecisionLink,
        now: Timestamp,
    ) -> Result<BotDecisionCard, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let (attempt, chat, message) = attempt_for_run_on(&tx, owner, link.bot, link.run)?;
        if let Some(existing) = card_on(&tx, "decision_id", &link.decision.to_storage())? {
            return replay(existing, link.run, attempt);
        }
        let invocation: Option<String> = tx
            .query_row(
                "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))=36 THEN id END
                 FROM tool_invocations WHERE attempt_id=?1 AND bot_source_kind=?2 AND bot_source_id=?3
                 AND status IN ('approval_required','running') AND approval='pending'",
                params![
                    attempt.to_storage(),
                    BotToolSourceKind::Approval.token(),
                    link.prompt.to_string()
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(bad)?
            .flatten();
        let invocation = ToolInvocationId::parse(&invocation.ok_or(BotError::NotFound)?)
            .ok_or(BotError::Storage)?;
        if card_on(&tx, "invocation_id", &invocation.to_storage())?.is_some() {
            return Err(BotError::Conflict);
        }
        link_on(
            &tx,
            link.decision,
            link.run,
            attempt,
            &chat,
            &message,
            invocation,
            link.revision,
            now,
        )?;
        let written =
            card_on(&tx, "decision_id", &link.decision.to_storage())?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
