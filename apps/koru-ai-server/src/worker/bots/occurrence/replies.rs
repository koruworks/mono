// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) fn exact_record<'a>(
    operation: &Operation,
    row: &BotRoutineStoredPreparation,
    observation: &'a RoutineClaimObservation,
) -> Result<&'a RoutineClaimRecord, RoutineBridgeRefusal> {
    let RoutineClaimObservation::Claimed { record } = observation else {
        return Err(RoutineBridgeRefusal::Unavailable);
    };
    if !record.valid()
        || !record.claim.matches_prepared(&row.prepared)
        || record.submission != row.prepared.submission
        || matches!(&operation.key.identity, Identity::Claim(expected) if *expected != record.claim)
    {
        return Err(RoutineBridgeRefusal::Conflict);
    }
    Ok(record)
}

enum PreparationView {
    Prepared(RoutinePrepared),
    Linked {
        preparation: RoutinePrepared,
        claim: RoutineClaimIdentity,
    },
    Closed {
        slot: RoutineSlot,
        hash: RoutineDigest32,
        reason: BotRoutineCloseReason,
    },
    Missing {
        slot: RoutineSlot,
        hash: RoutineDigest32,
    },
}
fn close(
    row: &BotRoutineStoredPreparation,
    closure: BotRoutineStoredClosure,
) -> Result<PreparationView, RoutineBridgeRefusal> {
    let reason = closure
        .preparation_reason()
        .ok_or(RoutineBridgeRefusal::Stale)?;
    Ok(PreparationView::Closed {
        slot: row.prepared.slot,
        hash: row.prepared.semantic_hash,
        reason,
    })
}
fn preparation(
    operation: &Operation,
    row: Option<&BotRoutineStoredPreparation>,
    observation: Option<&RoutineClaimObservation>,
) -> Result<PreparationView, RoutineBridgeRefusal> {
    let Some(row) = row else {
        let Identity::Semantic(hash) = &operation.key.identity else {
            return Err(RoutineBridgeRefusal::Invalid);
        };
        return Ok(PreparationView::Missing {
            slot: operation.key.slot,
            hash: *hash,
        });
    };
    match row.stage {
        BotRoutineStoredStage::Prepared => Ok(PreparationView::Prepared(row.prepared.clone())),
        BotRoutineStoredStage::Closed(closure) => close(row, closure),
        BotRoutineStoredStage::Linked => {
            let record = exact_record(
                operation,
                row,
                observation.ok_or(RoutineBridgeRefusal::Unavailable)?,
            )?;
            if let RoutineLinkState::Terminal { reason } = record.link_state {
                close(row, BotRoutineStoredClosure::ClaimedTerminal { reason })
            } else {
                Ok(PreparationView::Linked {
                    preparation: row.prepared.clone(),
                    claim: record.claim.clone(),
                })
            }
        }
    }
}

/// Only independent retained delegate evidence contributes claim timestamps.
pub(super) fn answer(
    operation: Operation,
    row: Option<&BotRoutineStoredPreparation>,
    observation: Option<&RoutineClaimObservation>,
) {
    let prepared = match &operation.reply {
        Reply::Prepare(_) | Reply::Read(_) => Some(preparation(&operation, row, observation)),
        _ => None,
    };
    let linked = match &operation.reply {
        Reply::Finalize(_) | Reply::Link(_) => Some((|| {
            let Some(row) = row else {
                if observation.is_some_and(|o| matches!(o, RoutineClaimObservation::Claimed { .. }))
                {
                    return Err(RoutineBridgeRefusal::Conflict);
                }
                return Ok(None);
            };
            let observed = observation.ok_or(RoutineBridgeRefusal::Unavailable)?;
            if !matches!(observed, RoutineClaimObservation::Claimed { .. }) {
                return if row.stage == BotRoutineStoredStage::Linked {
                    Err(RoutineBridgeRefusal::Conflict)
                } else {
                    Ok(None)
                };
            }
            let record = exact_record(&operation, row, observed)?;
            match record.link_state {
                RoutineLinkState::Terminal { .. } => Ok(Some(record.clone())),
                _ if row.stage == BotRoutineStoredStage::Linked => Ok(Some(record.clone())),
                RoutineLinkState::Linked {} => Err(RoutineBridgeRefusal::Conflict),
                _ => Ok(None),
            }
        })()),
        _ => None,
    };
    match operation.reply {
        Reply::Prepare(reply) => reply.send(Ok(match prepared.expect("preparation reply") {
            Ok(
                PreparationView::Prepared(preparation)
                | PreparationView::Linked { preparation, .. },
            ) => PrepareOccurrenceReply::Prepared { preparation },
            Ok(PreparationView::Closed { slot, hash, reason }) => PrepareOccurrenceReply::Closed {
                slot,
                semantic_hash: hash,
                reason,
            },
            Ok(PreparationView::Missing { .. }) => PrepareOccurrenceReply::Refused {
                code: RoutineBridgeRefusal::Unavailable,
            },
            Err(code) => PrepareOccurrenceReply::Refused { code },
        })),
        Reply::Read(reply) => reply.send(Ok(match prepared.expect("preparation reply") {
            Ok(PreparationView::Prepared(preparation)) => {
                ReadPreparationReply::Prepared { preparation }
            }
            Ok(PreparationView::Linked { preparation, claim }) => ReadPreparationReply::Linked {
                slot: preparation.slot,
                semantic_hash: preparation.semantic_hash,
                claim,
            },
            Ok(PreparationView::Closed { slot, hash, reason }) => ReadPreparationReply::Closed {
                slot,
                semantic_hash: hash,
                reason,
            },
            Ok(PreparationView::Missing { slot, hash }) => ReadPreparationReply::Missing {
                slot,
                semantic_hash: hash,
            },
            Err(code) => ReadPreparationReply::Refused { code },
        })),
        Reply::Finalize(reply) => reply.send(Ok(match linked.expect("link reply") {
            Ok(Some(record)) => match record.link_state {
                RoutineLinkState::Terminal { reason } => FinalizeOccurrenceReply::Terminal {
                    claim: record.claim,
                    reason,
                },
                _ => FinalizeOccurrenceReply::Linked {
                    claim: record.claim,
                },
            },
            Ok(None) => FinalizeOccurrenceReply::Refused {
                code: RoutineBridgeRefusal::Unavailable,
            },
            Err(code) => FinalizeOccurrenceReply::Refused { code },
        })),
        Reply::Link(reply) => reply.send(Ok(match linked.expect("link reply") {
            Ok(Some(record)) => match record.link_state {
                RoutineLinkState::Terminal { reason } => ReadOccurrenceLinkReply::Terminal {
                    claim: record.claim,
                    reason,
                },
                _ => ReadOccurrenceLinkReply::Linked {
                    claim: record.claim,
                },
            },
            Ok(None) => match operation.key.identity {
                Identity::Claim(claim) => ReadOccurrenceLinkReply::Missing { claim },
                _ => ReadOccurrenceLinkReply::Refused {
                    code: RoutineBridgeRefusal::Invalid,
                },
            },
            Err(code) => ReadOccurrenceLinkReply::Refused { code },
        })),
        Reply::Recovery => {}
    }
}
