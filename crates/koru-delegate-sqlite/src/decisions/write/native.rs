// SPDX-License-Identifier: MPL-2.0

use super::*;

fn key(response: &DecisionNativeResponse) -> &DecisionNativeResponseKey {
    match response {
        DecisionNativeResponse::Prepared { key }
        | DecisionNativeResponse::Unconfirmed { key }
        | DecisionNativeResponse::Acknowledged { key, .. } => key,
    }
}

/// Record a result of the original response dispatch. This is reconciliation,
/// not permission to dispatch: fresh current policy/deadline belongs before the
/// external answer. A late acknowledgment cannot erase Stop's cleanup debt.
pub(crate) fn record_native_answer_on(
    tx: &Transaction<'_>,
    owner: BotOwner,
    run: RunId,
    response: &DecisionNativeResponse,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    valid(response)?;
    authority::owner(tx, owner, run)?;
    let identity = key(response);
    let old = required(tx, owner, run, identity.decision)?;
    let prior = old.native_response.as_ref().ok_or(BotLedgerError::Stale)?;
    require(key(prior) == identity)?;
    if prior == response {
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    if matches!(prior, DecisionNativeResponse::Acknowledged { .. })
        || matches!(response, DecisionNativeResponse::Prepared { .. })
    {
        return Err(BotLedgerError::Conflict);
    }
    let answered = old
        .receipt
        .answer
        .as_ref()
        .ok_or(BotLedgerError::Conflict)?
        .answered_at_ms;
    let (state, acknowledgment, observed) = match response {
        DecisionNativeResponse::Unconfirmed { .. } => ("unconfirmed", None, now),
        DecisionNativeResponse::Acknowledged { observed_at_ms, .. } => {
            require(*observed_at_ms >= answered && *observed_at_ms <= now)?;
            (
                "acknowledged",
                Some(encoded(response, DECISION_RECEIPT_BYTES)?),
                *observed_at_ms,
            )
        }
        DecisionNativeResponse::Prepared { .. } => unreachable!(),
    };
    let updated = now.max(old.receipt.updated_at_ms).max(observed);
    tx.execute(
        "UPDATE bot_decision_native_answers SET state=?2,acknowledgment_json=?3,updated_at_ms=max(updated_at_ms,?4)
         WHERE decision_id=?1 AND state<>'acknowledged'",
        params![identity.decision.to_storage(), state, acknowledgment, updated],
    ).map_err(sql)?;
    let active = matches!(
        old.receipt.state.phase,
        DecisionPhase::NativeDispatch | DecisionPhase::NativeUnknown
    );
    if active {
        if state == "acknowledged" {
            let grants = identity.choice.intent.grants();
            let authorization = if grants { "consumed" } else { "denied" };
            let reason = if grants { "completed" } else { "denied" };
            tx.execute(
                "UPDATE bot_decisions SET authorization=?2,phase='settled',revision=?3,
                 consumed_at_ms=?4,closure_reason=?5,settled_at_ms=?6,updated_at_ms=?6
                 WHERE decision_id=?1 AND revision=?7",
                params![
                    identity.decision.to_storage(),
                    authorization,
                    next(old.receipt.state.revision)?.get(),
                    grants.then_some(observed),
                    reason,
                    updated,
                    old.receipt.state.revision.get()
                ],
            )
            .map_err(sql)?;
            // The runtime wait remains Fenced and retains its occupied slot
            // until all owed projections acknowledge and its handle returns.
            // Reject settles only this answer; it does not terminate the Run.
        } else {
            tx.execute(
                "UPDATE bot_decisions SET phase='native_unknown',revision=?2,updated_at_ms=?3
                 WHERE decision_id=?1 AND revision=?4",
                params![
                    identity.decision.to_storage(),
                    next(old.receipt.state.revision)?.get(),
                    updated,
                    old.receipt.state.revision.get()
                ],
            )
            .map_err(sql)?;
        }
    }
    done(
        tx,
        owner,
        run,
        identity.decision,
        crate::WriteOutcome::Applied,
    )
}
