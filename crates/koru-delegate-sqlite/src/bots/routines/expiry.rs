// SPDX-License-Identifier: MPL-2.0

//! No intent/admission/link insertion is reachable through expiry authority.
use super::{claims::skip, rows::*, *};
use koru_delegate_core::*;

impl RunsDb {
    pub fn record_expired_routine(
        &mut self,
        facts: RoutineExpiredClaimFacts,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        self.write_with_error(WriteOp::RecordExpiredRoutine, |tx, now| {
            let slot = facts.slot();
            let route = subject(tx, &slot)?;
            if route.workspace != facts.workspace()
                || route.creation_request != facts.creation_request()
            {
                return Err(BotLedgerError::Fenced);
            }
            let old = observation(tx, &slot, now)?;
            if !matches!(old, RoutineClaimObservation::Absent { .. }) {
                let missed = facts
                    .representative_miss()
                    .map(|miss| {
                        observation(
                            tx,
                            &RoutineSlot {
                                occurrence_ms: miss.occurrence_ms,
                                ..slot
                            },
                            now,
                        )
                    })
                    .transpose()?;
                return Ok((
                    old,
                    match missed {
                        Some(RoutineClaimObservation::Skipped { record }) => Some(record),
                        _ => None,
                    },
                ));
            }
            // A rollback or arithmetic overflow never becomes positive expiry.
            let expired =
                |at| now >= at && now.checked_sub(at).is_some_and(|elapsed| elapsed >= 60_000);
            if !expired(slot.occurrence_ms)
                || facts
                    .representative_miss()
                    .is_some_and(|miss| !expired(miss.occurrence_ms))
            {
                return Err(BotLedgerError::Fenced);
            }
            let record = skip(
                tx,
                slot,
                facts.definition().revision,
                facts.planned_run(),
                RoutineSkipOutcome::SkippedMissed {},
                now,
            )?;
            let missed = facts
                .representative_miss()
                .map(|miss| {
                    skip(
                        tx,
                        RoutineSlot {
                            occurrence_ms: miss.occurrence_ms,
                            ..slot
                        },
                        facts.definition().revision,
                        miss.planned_run,
                        RoutineSkipOutcome::SkippedMissed {},
                        now,
                    )
                })
                .transpose()?;
            Ok((RoutineClaimObservation::Skipped { record }, missed))
        })
    }
}

#[cfg(test)]
mod tests;
