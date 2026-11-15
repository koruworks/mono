// SPDX-License-Identifier: MPL-2.0

//! Bounded cleanup identity discovery and Source/carrier atomic ownership.
use super::*;

/// Historical qualification deliberately does not require an open current wait:
/// later cleanup/settlement must not invalidate an exact retained Source pair.
pub(super) fn origin(
    conn: &Connection,
    projection: &DecisionProjectionRecord,
) -> Result<Option<DecisionCleanupOrigin>, BotLedgerError> {
    let RunProjectionTargetV1::Control {
        action,
        wait_phase: BotWaitPhase::CleanupPending,
        reason,
        fence,
        disposition,
        ..
    } = &projection.target
    else {
        return Ok(None);
    };
    let wait =
        super::super::rows::by_wait(conn, projection.identity.run, projection.identity.wait)?
            .ok_or(BotLedgerError::Stale)?;
    require(
        wait.reason == *reason
            && wait.origin.owner == fence.owner
            && wait.origin.bot == fence.bot
            && wait.origin.run == fence.run,
    )?;
    match action {
        DecisionControlAction::CleanupRequested {
            cause,
            expected_wait_revision,
        } => {
            require(*expected_wait_revision < wait.revision)?;
            Ok(Some(DecisionCleanupOrigin::Requested {
                wait: wait.wait,
                expected_wait_revision: *expected_wait_revision,
                cause: *cause,
            }))
        }
        DecisionControlAction::Preserve {} => match reason {
            BotWaitReason::Decision { decision } => {
                let record = super::super::rows::read(conn, fence.owner, fence.run, *decision)?
                    .ok_or(BotLedgerError::Stale)?;
                let Some(DecisionDisposition::Open { state }) = disposition else {
                    return Ok(None);
                };
                if state.kind != DecisionKind::KoruEdit
                    || state.authorization != DecisionAuthorization::Denied
                    || state.phase != DecisionPhase::StopPending
                {
                    return Ok(None);
                }
                let answer = record
                    .receipt
                    .answer
                    .as_ref()
                    .ok_or(BotLedgerError::Fenced)?;
                require(
                    matches!(answer.choice, DecisionAnswerChoice::Deny {})
                        && state.revision >= answer.committed_revision
                        && projection.identity.decision.as_ref().is_some_and(|r| {
                            r.decision == *decision && r.revision == state.revision
                        }),
                )?;
                Ok(Some(DecisionCleanupOrigin::Denied {
                    wait: wait.wait,
                    decision: *decision,
                    answer: answer.request,
                    answer_revision: answer.committed_revision,
                }))
            }
            _ => {
                require(
                    projection.identity.decision.is_none()
                        && disposition.is_none()
                        && *fence == wait.origin,
                )?;
                Ok(Some(DecisionCleanupOrigin::NoDecision {
                    wait: wait.wait,
                    reason: *reason,
                    origin: wait.origin,
                }))
            }
        },
        _ => Ok(None),
    }
}

pub(super) fn current(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Option<DecisionCleanupOwnership>, BotLedgerError> {
    let Some(observed) = super::super::observation::execution(conn, owner, run)? else {
        return Ok(None);
    };
    if observed.wait.phase != BotWaitPhase::CleanupPending || observed.wait.cleanup.is_some() {
        return Ok(None);
    }
    let latest = rows::latest(conn, run)?.ok_or(BotLedgerError::Stale)?;
    let projection = read(conn, owner, run, latest)?.ok_or(BotLedgerError::Stale)?;
    require(projection.identity.wait == observed.wait.wait)?;
    let origin = origin(conn, &projection)?.ok_or(BotLedgerError::Fenced)?;
    Ok(Some(DecisionCleanupOwnership {
        expected: observed.expected,
        projection: latest,
        origin,
    }))
}

pub(super) fn action(origin: &DecisionCleanupOrigin) -> DecisionControlAction {
    match origin {
        DecisionCleanupOrigin::Requested {
            cause,
            expected_wait_revision,
            ..
        } => DecisionControlAction::CleanupRequested {
            cause: *cause,
            expected_wait_revision: *expected_wait_revision,
        },
        _ => DecisionControlAction::Preserve {},
    }
}

/// At most the latest target and its exact predecessor. During Source+carry
/// insertion, the accepted Source is newest; it cannot supply the owner itself.
pub(super) fn retained_for_carry(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Option<DecisionCleanupOrigin>, BotLedgerError> {
    let Some(latest) = rows::latest(conn, run)? else {
        return Ok(None);
    };
    let latest = read(conn, owner, run, latest)?.ok_or(BotLedgerError::Stale)?;
    if let Some(origin) = origin(conn, &latest)? {
        return Ok(Some(origin));
    }
    if latest.identity.kind != DecisionProjectionKind::Source {
        return Ok(None);
    }
    let Some(prior) = &latest.target.predecessor().projection else {
        return Ok(None);
    };
    let prior = read(conn, owner, run, prior.transition)?.ok_or(BotLedgerError::Stale)?;
    require(prior.identity.wait == latest.identity.wait)?;
    origin(conn, &prior)
}

pub(super) fn carry(
    tx: &Transaction<'_>,
    owner: BotOwner,
    source: &DecisionProjectionRecord,
    origin: &DecisionCleanupOrigin,
    now: i64,
) -> Result<(), BotLedgerError> {
    let run = source.identity.run;
    let wait_id = source.identity.wait;
    let wait = super::super::rows::by_wait(tx, run, wait_id)?.ok_or(BotLedgerError::Stale)?;
    require(wait.phase == BotWaitPhase::CleanupPending && wait.cleanup.is_none())?;
    let disposition = match wait.reason {
        BotWaitReason::Decision { decision } => {
            let record =
                super::super::rows::read(tx, owner, run, decision)?.ok_or(BotLedgerError::Stale)?;
            Some(control::disposition(&record.receipt)?)
        }
        _ => None,
    };
    let context = control::context(tx, owner, run, wait_id)?;
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: context.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event: None,
            status: context.predecessor.status.clone(),
            failure_reason: context.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: *source.target.fence(),
        disposition,
        wait_phase: wait.phase,
        reason: wait.reason,
        action: action(origin),
        retained_bookkeeping: context.bookkeeping,
        flush: DecisionList::default(),
        delivery: None,
    };
    let carrier = insert::insert_on(
        tx,
        owner,
        DecisionTransitionId::mint(),
        wait_id,
        &context.base,
        &target,
        now,
    )?;
    require(
        carrier.identity.sequence == next(source.identity.sequence)?
            && self::origin(tx, &carrier)?.as_ref() == Some(origin),
    )
}

pub(super) fn replay_pair(
    conn: &Connection,
    owner: BotOwner,
    source: &DecisionProjectionRecord,
) -> Result<(), BotLedgerError> {
    let Some(previous) = &source.target.predecessor().projection else {
        return Ok(());
    };
    let previous = read(conn, owner, source.identity.run, previous.transition)?
        .ok_or(BotLedgerError::Stale)?;
    let Some(origin) = origin(conn, &previous)? else {
        return Ok(());
    };
    let transition: String=conn.query_row("SELECT CASE WHEN typeof(transition_id)='text' AND length(CAST(transition_id AS BLOB))=36 THEN transition_id END FROM bot_decision_publications WHERE run_id=?1 AND run_sequence=?2",
        params![source.identity.run.to_storage(),next(source.identity.sequence)?.get()],|r|r.get(0)).map_err(sql)?;
    let carrier = read(
        conn,
        owner,
        source.identity.run,
        transition.parse().map_err(|_| BotLedgerError::Invalid)?,
    )?
    .ok_or(BotLedgerError::Stale)?;
    require(
        carrier.identity.wait == source.identity.wait
            && carrier.target.predecessor() == &insert::prior(source)?
            && self::origin(conn, &carrier)?.as_ref() == Some(&origin),
    )
}

impl RunsDb {
    pub fn decision_cleanup_ownership(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionCleanupOwnership>, BotLedgerError> {
        current(self.conn(), owner, run)
    }
}
