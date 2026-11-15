// SPDX-License-Identifier: MPL-2.0

//! Closed controls use retained targets, never arbitrary caller effects.
use super::*;

pub(super) struct ControlContext {
    pub base: DecisionPublicationBase,
    pub predecessor: DecisionProjectionPredecessor,
    pub bookkeeping: Option<DecisionSourceBookkeeping>,
}

/// Same-wait baseline without a document/history import: one Applied body and
/// the at-most-two open prefix. Initial wait publication must already exist.
pub(super) fn context(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    wait: BotWaitId,
) -> Result<ControlContext, BotLedgerError> {
    let mut open = prefix(conn, owner, run)?;
    require(open.iter().all(|r| r.identity.wait == wait))?;
    let applied = latest_applied_on(conn, owner, run)?;
    let base = if let Some(applied) = applied.as_ref().filter(|r| r.identity.wait == wait) {
        let predecessor = insert::prior(applied)?;
        DecisionPublicationBase {
            workspace: applied.identity.workspace,
            document: applied.identity.document,
            source_schema: 5u8.try_into().map_err(|_| BotLedgerError::Invalid)?,
            observed_created_at_ms: predecessor.observed_created_at_ms,
            timeline_len: predecessor.timeline_start,
            subject_digest: applied.identity.subject_digest,
            predecessor,
            bookkeeping: applied
                .target
                .resulting_bookkeeping()
                .map_err(|_| BotLedgerError::Invalid)?,
        }
    } else {
        let first = open.first().ok_or(BotLedgerError::Stale)?;
        DecisionPublicationBase {
            workspace: first.identity.workspace,
            document: first.identity.document,
            source_schema: first.identity.source_schema,
            observed_created_at_ms: first.target.predecessor().observed_created_at_ms,
            timeline_len: first.target.predecessor().timeline_start,
            subject_digest: first.identity.subject_digest,
            predecessor: first.target.predecessor().clone(),
            bookkeeping: match &first.target {
                RunProjectionTargetV1::Control {
                    retained_bookkeeping,
                    ..
                } => retained_bookkeeping.clone(),
                // No pre-event tally can be reconstructed from an initial
                // Source. The mandatory initial wait Control owns that base.
                RunProjectionTargetV1::Source { .. } => return Err(BotLedgerError::Fenced),
            },
        }
    };
    // Replacement excludes precisely the unprepared Control being superseded.
    if open.last().is_some_and(|r| {
        r.identity.kind == DecisionProjectionKind::Control
            && matches!(r.state, DecisionProjectionState::Pending)
    }) {
        open.pop();
    }
    let (predecessor, bookkeeping) = match open.last() {
        Some(last) => (
            insert::prior(last)?,
            last.target
                .resulting_bookkeeping()
                .map_err(|_| BotLedgerError::Invalid)?,
        ),
        None => (base.predecessor.clone(), base.bookkeeping.clone()),
    };
    Ok(ControlContext {
        base,
        predecessor,
        bookkeeping,
    })
}
