// SPDX-License-Identifier: MPL-2.0

//! Schedule claims: one slot, one run (§7.7).
//!
//! A slot is `(ScheduleId, occurrence instant)`. The claim and the `RunId`
//! it plans are one row written in one transaction, so a crash after
//! claiming leaves a run id to look up and §7.6 classifies it; the revision
//! in force when the slot was claimed is recorded on it and is never part
//! of the key, so editing a template cannot turn one slot into two. A slot
//! that was skipped before launch — missed (D25) or busy (D34) — is recorded
//! already settled, because "nothing fired and here is why" is a fact a
//! person reads in the schedule's history. Retirement after a valid claim is
//! never a skip and cannot cancel that launch.

use koru_delegate_core::{DelegationRequest, RunId, ScheduleId, WorkspaceId};
use rusqlite::{OptionalExtension, params};

use crate::{
    LedgerError, RunsDb, SlotOutcome, SlotState, WriteOp, WriteOutcome, intents::parse_run,
};

/// A slot this device just claimed, with the run it plans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClaimedSlot {
    pub schedule: ScheduleId,
    pub occurrence_ms: i64,
    pub revision: u32,
    pub run: RunId,
    pub claimed_at_ms: i64,
}

/// A slot as the ledger holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotClaim {
    pub schedule: ScheduleId,
    pub occurrence_ms: i64,
    pub revision: u32,
    pub run: RunId,
    pub state: SlotState,
    pub busy_holder: Option<RunId>,
    pub claimed_at_ms: i64,
}

/// A settled skipped slot. The schema retains a planned id for every slot,
/// but no `run_intents` row exists because skipped work is never a run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SkippedSlot {
    pub claim: SlotClaim,
}

impl RunsDb {
    /// Atomically preserve both facts found after a long absence: the
    /// currently runnable slot and one bounded representative miss. Neither
    /// row can advance the observation cursor past the other without both
    /// having committed.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_scheduled_run_with_missed(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        request: &DelegationRequest,
        workspace: WorkspaceId,
        missed_occurrence_ms: i64,
        missed_run: RunId,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError> {
        let claimed_at_ms = self.now_ms();
        self.claim_scheduled_run_with_missed_at(
            schedule,
            occurrence_ms,
            revision,
            request,
            workspace,
            missed_occurrence_ms,
            missed_run,
            claimed_at_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_scheduled_run_with_missed_at(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        request: &DelegationRequest,
        workspace: WorkspaceId,
        missed_occurrence_ms: i64,
        missed_run: RunId,
        claimed_at_ms: i64,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError> {
        if revision == 0 {
            return Err(LedgerError::RevisionZero);
        }
        let payload =
            serde_json::to_string(request).map_err(|_| LedgerError::PayloadUnencodable)?;
        self.write(WriteOp::ClaimScheduledRun, |tx, _now| {
            crate::bots::routines::deny_schedule(tx, schedule)?;
            let claimed = match read(tx, schedule, occurrence_ms)? {
                Some(existing) => match existing.state {
                    SlotState::Claimed => ClaimedSlot {
                        schedule,
                        occurrence_ms,
                        revision: existing.revision,
                        run: existing.run,
                        claimed_at_ms: existing.claimed_at_ms,
                    },
                    SlotState::Settled(outcome) => {
                        return Err(LedgerError::SlotOutcomeRecorded { outcome });
                    }
                },
                None => {
                    tx.execute(
                        "INSERT INTO run_intents
                           (run_id, workspace_id, payload_json, state, created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
                        params![
                            request.run.to_storage(),
                            workspace.to_storage(),
                            payload,
                            claimed_at_ms
                        ],
                    )
                    .map_err(|error| LedgerError::sql("record a scheduled intent", error))?;
                    tx.execute(
                        "INSERT INTO schedule_claims
                           (schedule_id, occurrence_ms, revision, run_id, outcome, claimed_at_ms)
                         VALUES (?1, ?2, ?3, ?4, 'claimed', ?5)",
                        params![
                            schedule.to_storage(),
                            occurrence_ms,
                            revision,
                            request.run.to_storage(),
                            claimed_at_ms
                        ],
                    )
                    .map_err(|error| LedgerError::sql("claim a scheduled run", error))?;
                    ClaimedSlot {
                        schedule,
                        occurrence_ms,
                        revision,
                        run: request.run,
                        claimed_at_ms,
                    }
                }
            };
            let missed = claim_skip_in_tx(
                tx,
                claimed_at_ms,
                schedule,
                missed_occurrence_ms,
                revision,
                missed_run,
                SlotOutcome::SkippedMissed,
                None,
            )?;
            Ok((claimed, missed))
        })
    }

    /// Atomically record a due-slot skip and the representative older miss
    /// found in the same bounded evaluation.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_skip_with_missed(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        planned_run: RunId,
        outcome: SlotOutcome,
        busy_holder: Option<RunId>,
        missed_occurrence_ms: i64,
        missed_run: RunId,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError> {
        let claimed_at_ms = self.now_ms();
        self.claim_skip_with_missed_at(
            schedule,
            occurrence_ms,
            revision,
            planned_run,
            outcome,
            busy_holder,
            missed_occurrence_ms,
            missed_run,
            claimed_at_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_skip_with_missed_at(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        planned_run: RunId,
        outcome: SlotOutcome,
        busy_holder: Option<RunId>,
        missed_occurrence_ms: i64,
        missed_run: RunId,
        claimed_at_ms: i64,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError> {
        validate_skip(outcome, busy_holder)?;
        if revision == 0 {
            return Err(LedgerError::RevisionZero);
        }
        self.write(WriteOp::ClaimSkip, |tx, _now| {
            crate::bots::routines::deny_schedule(tx, schedule)?;
            let due = claim_skip_in_tx(
                tx,
                claimed_at_ms,
                schedule,
                occurrence_ms,
                revision,
                planned_run,
                outcome,
                busy_holder,
            )?;
            let missed = claim_skip_in_tx(
                tx,
                claimed_at_ms,
                schedule,
                missed_occurrence_ms,
                revision,
                missed_run,
                SlotOutcome::SkippedMissed,
                None,
            )?;
            Ok((due, missed))
        })
    }

    /// Atomically reserve one slot and write the complete pending run intent.
    /// A lost answer returns the original claim on retry, even if the caller
    /// pre-minted another request meanwhile.
    pub fn claim_scheduled_run(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<ClaimedSlot, LedgerError> {
        let claimed_at_ms = self.now_ms();
        self.claim_scheduled_run_at(
            schedule,
            occurrence_ms,
            revision,
            request,
            workspace,
            claimed_at_ms,
        )
    }

    pub fn claim_scheduled_run_at(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        request: &DelegationRequest,
        workspace: WorkspaceId,
        claimed_at_ms: i64,
    ) -> Result<ClaimedSlot, LedgerError> {
        if revision == 0 {
            return Err(LedgerError::RevisionZero);
        }
        let payload =
            serde_json::to_string(request).map_err(|_| LedgerError::PayloadUnencodable)?;
        self.write(WriteOp::ClaimScheduledRun, |tx, _now| {
            crate::bots::routines::deny_schedule(tx, schedule)?;
            if let Some(existing) = read(tx, schedule, occurrence_ms)? {
                return match existing.state {
                    SlotState::Claimed => Ok(ClaimedSlot {
                        schedule,
                        occurrence_ms,
                        revision: existing.revision,
                        run: existing.run,
                        claimed_at_ms: existing.claimed_at_ms,
                    }),
                    SlotState::Settled(outcome) => {
                        Err(LedgerError::SlotOutcomeRecorded { outcome })
                    }
                };
            }
            tx.execute(
                "INSERT INTO run_intents
                   (run_id, workspace_id, payload_json, state, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
                params![
                    request.run.to_storage(),
                    workspace.to_storage(),
                    payload,
                    claimed_at_ms
                ],
            )
            .map_err(|error| LedgerError::sql("record a scheduled intent", error))?;
            tx.execute(
                "INSERT INTO schedule_claims
                   (schedule_id, occurrence_ms, revision, run_id, outcome, claimed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'claimed', ?5)",
                params![
                    schedule.to_storage(),
                    occurrence_ms,
                    revision,
                    request.run.to_storage(),
                    claimed_at_ms
                ],
            )
            .map_err(|error| LedgerError::sql("claim a scheduled run", error))?;
            Ok(ClaimedSlot {
                schedule,
                occurrence_ms,
                revision,
                run: request.run,
                claimed_at_ms,
            })
        })
    }

    /// Atomically reserve and settle a slot that must not start. There is no
    /// intermediate `claimed` state and no pending intent for restart to
    /// mistake for launch work.
    pub fn claim_skip(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        planned_run: RunId,
        outcome: SlotOutcome,
        busy_holder: Option<RunId>,
    ) -> Result<SkippedSlot, LedgerError> {
        let claimed_at_ms = self.now_ms();
        self.claim_skip_at(
            schedule,
            occurrence_ms,
            revision,
            planned_run,
            outcome,
            busy_holder,
            claimed_at_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_skip_at(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
        planned_run: RunId,
        outcome: SlotOutcome,
        busy_holder: Option<RunId>,
        claimed_at_ms: i64,
    ) -> Result<SkippedSlot, LedgerError> {
        if revision == 0 {
            return Err(LedgerError::RevisionZero);
        }
        validate_skip(outcome, busy_holder)?;
        self.write(WriteOp::ClaimSkip, |tx, _now| {
            crate::bots::routines::deny_schedule(tx, schedule)?;
            claim_skip_in_tx(
                tx,
                claimed_at_ms,
                schedule,
                occurrence_ms,
                revision,
                planned_run,
                outcome,
                busy_holder,
            )
        })
    }

    /// Low-level diagnostic/test primitive retained for the schema-1 crash
    /// harness. Production scheduling uses `claim_scheduled_run`; this method
    /// intentionally does not create a recoverable intent.
    ///
    /// Claim the slot and mint its run in one transaction. A slot already
    /// claimed — under any revision — is [`LedgerError::SlotAlreadyClaimed`]
    /// naming the run that holds it.
    pub fn claim_slot(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        revision: u32,
    ) -> Result<ClaimedSlot, LedgerError> {
        if revision == 0 {
            return Err(LedgerError::RevisionZero);
        }
        self.write(WriteOp::ClaimSlot, |tx, now| {
            crate::bots::routines::deny_schedule(tx, schedule)?;
            if let Some(existing) = read(tx, schedule, occurrence_ms)? {
                return Err(LedgerError::SlotAlreadyClaimed {
                    run: existing.run,
                    revision: existing.revision,
                });
            }
            let run = RunId::mint();
            tx.execute(
                "INSERT INTO schedule_claims
                   (schedule_id, occurrence_ms, revision, run_id, outcome, claimed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'claimed', ?5)",
                params![
                    schedule.to_storage(),
                    occurrence_ms,
                    revision,
                    run.to_storage(),
                    now
                ],
            )
            .map_err(|error| LedgerError::sql("claim a slot", error))?;
            Ok(ClaimedSlot {
                schedule,
                occurrence_ms,
                revision,
                run,
                claimed_at_ms: now,
            })
        })
    }

    /// Settle a claimed slot. The same outcome twice is already applied; a
    /// different one is [`LedgerError::SlotOutcomeRecorded`] — a slot's
    /// history is not rewritten.
    pub fn record_outcome(
        &mut self,
        schedule: ScheduleId,
        occurrence_ms: i64,
        outcome: SlotOutcome,
    ) -> Result<WriteOutcome, LedgerError> {
        if outcome != SlotOutcome::Fired {
            return Err(LedgerError::InvalidSettlementOutcome);
        }
        self.write(WriteOp::RecordOutcome, |tx, _| {
            let claim = read(tx, schedule, occurrence_ms)?.ok_or(LedgerError::UnknownSlot)?;
            match claim.state {
                SlotState::Claimed => {
                    tx.execute(
                        "UPDATE schedule_claims SET outcome = ?3
                          WHERE schedule_id = ?1 AND occurrence_ms = ?2",
                        params![schedule.to_storage(), occurrence_ms, outcome.token()],
                    )
                    .map_err(|error| LedgerError::sql("record a slot outcome", error))?;
                    Ok(WriteOutcome::Applied)
                }
                SlotState::Settled(recorded) if recorded == outcome => {
                    Ok(WriteOutcome::AlreadyApplied)
                }
                SlotState::Settled(recorded) => {
                    Err(LedgerError::SlotOutcomeRecorded { outcome: recorded })
                }
            }
        })
    }

    /// One slot, if this device claimed it.
    pub fn slot(
        &self,
        schedule: ScheduleId,
        occurrence_ms: i64,
    ) -> Result<Option<SlotClaim>, LedgerError> {
        read(self.conn(), schedule, occurrence_ms)
    }

    /// Every slot claimed for a schedule, earliest occurrence first.
    pub fn slots_of(&self, schedule: ScheduleId) -> Result<Vec<SlotClaim>, LedgerError> {
        let mut statement = self
            .conn()
            .prepare(
                "SELECT occurrence_ms, revision, run_id, outcome, busy_holder_run_id, claimed_at_ms
                   FROM schedule_claims WHERE schedule_id = ?1 ORDER BY occurrence_ms",
            )
            .map_err(|error| LedgerError::sql("list slots", error))?;
        let rows = statement
            .query_map(params![schedule.to_storage()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|error| LedgerError::sql("list slots", error))?;
        let mut slots = Vec::new();
        for row in rows {
            let (occurrence_ms, revision, run, outcome, busy_holder, claimed_at_ms) =
                row.map_err(|error| LedgerError::sql("list slots", error))?;
            slots.push(typed(
                schedule,
                occurrence_ms,
                revision,
                &run,
                &outcome,
                busy_holder.as_deref(),
                claimed_at_ms,
            )?);
        }
        Ok(slots)
    }

    /// Claims whose pending intents must be completed forward after restart.
    pub fn unsettled_claims(&self) -> Result<Vec<SlotClaim>, LedgerError> {
        self.read_slots(
            "WHERE outcome = 'claimed' ORDER BY claimed_at_ms, schedule_id, occurrence_ms",
            [],
            false,
        )
    }

    /// Executable ordinary recovery excludes every retained Routine marker.
    /// Canonical historical reads above deliberately remain unchanged.
    pub fn ordinary_unsettled_claims(&self) -> Result<Vec<SlotClaim>, LedgerError> {
        self.read_slots(
            "WHERE outcome = 'claimed' ORDER BY claimed_at_ms, schedule_id, occurrence_ms",
            [],
            true,
        )
    }

    /// Exact-run deny-only classification for ordinary occurrence publication.
    /// A known negative is not permission to execute; retained Routine markers
    /// and malformed evidence reuse the same owner as ordinary recovery.
    pub fn validate_ordinary_schedule_run(&self, run: RunId) -> Result<(), LedgerError> {
        crate::bots::routines::deny_run(self.conn(), run)
    }

    /// The schedule slot that originated one run, including after restart.
    pub fn slot_for_run(&self, run: RunId) -> Result<Option<SlotClaim>, LedgerError> {
        let row = self.conn().query_row(
            "SELECT schedule_id, occurrence_ms, revision, outcome, busy_holder_run_id, claimed_at_ms FROM schedule_claims WHERE run_id = ?1",
            params![run.to_storage()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, i64>(5)?)),
        ).optional().map_err(|error| LedgerError::sql("find a slot by run", error))?;
        row.map(
            |(schedule, occurrence, revision, outcome, busy_holder, at)| {
                let schedule = schedule.parse().map_err(|_| LedgerError::Corrupt {
                    table: "schedule_claims",
                    column: "schedule_id",
                })?;
                typed(
                    schedule,
                    occurrence,
                    revision,
                    &run.to_storage(),
                    &outcome,
                    busy_holder.as_deref(),
                    at,
                )
            },
        )
        .transpose()
    }

    /// Restart cursor without a schema field: a missed skip advances through
    /// observation time; every other settled slot advances through occurrence.
    pub fn observed_through_ms(&self, schedule: ScheduleId) -> Result<Option<i64>, LedgerError> {
        self.conn().query_row(
            "SELECT MAX(CASE WHEN outcome = 'skipped_missed' THEN claimed_at_ms ELSE occurrence_ms END)
               FROM schedule_claims WHERE schedule_id = ?1 AND outcome <> 'claimed'",
            params![schedule.to_storage()],
            |row| row.get(0),
        ).map_err(|error| LedgerError::sql("read the schedule cursor", error))
    }

    fn read_slots<const N: usize>(
        &self,
        suffix: &str,
        params: [rusqlite::types::Value; N],
        ordinary_only: bool,
    ) -> Result<Vec<SlotClaim>, LedgerError> {
        let columns = if ordinary_only {
            "CASE WHEN typeof(c.schedule_id)='text' AND length(CAST(c.schedule_id AS BLOB))=36 THEN c.schedule_id END,
             c.occurrence_ms,c.revision,
             CASE WHEN typeof(c.run_id)='text' AND length(CAST(c.run_id AS BLOB))=36 THEN c.run_id END,
             c.outcome,
             CASE WHEN typeof(c.busy_holder_run_id)='text' AND length(CAST(c.busy_holder_run_id AS BLOB))=36 THEN c.busy_holder_run_id END,
             c.claimed_at_ms,
             EXISTS(SELECT 1 FROM bot_routine_routes r WHERE r.schedule_id=c.schedule_id)
             OR EXISTS(SELECT 1 FROM bot_routine_links l WHERE l.run_id=c.run_id)
             OR EXISTS(SELECT 1 FROM bot_admissions a WHERE a.run_id=c.run_id AND a.submission_version=2),
             c.busy_holder_run_id IS NULL OR (typeof(c.busy_holder_run_id)='text' AND length(CAST(c.busy_holder_run_id AS BLOB))=36)"
        } else {
            "c.schedule_id,c.occurrence_ms,c.revision,c.run_id,c.outcome,c.busy_holder_run_id,c.claimed_at_ms,0,1"
        };
        let sql = format!("SELECT {columns} FROM schedule_claims c {suffix}");
        let mut statement = self
            .conn()
            .prepare(&sql)
            .map_err(|error| LedgerError::sql("list claims", error))?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(params))
            .map_err(|error| LedgerError::sql("list claims", error))?;
        let mut claims = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| LedgerError::sql("list claims", e))?
        {
            let cell_error = |e| LedgerError::sql("list claims", e);
            // Deny indexed positive markers before allocating any malformed
            // slot content. Remaining marker/shape checks need only a bounded
            // Run identity, not a second full intent read or slot decoding.
            if ordinary_only && row.get::<_, bool>(7).map_err(cell_error)? {
                continue;
            }
            let run: String = row.get(3).map_err(cell_error)?;
            if ordinary_only {
                let identity = run.parse().map_err(|_| LedgerError::Corrupt {
                    table: "schedule_claims",
                    column: "run_id",
                })?;
                match crate::bots::routines::deny_run(self.conn(), identity) {
                    Ok(()) => {}
                    Err(LedgerError::InvalidScheduleAuthority) => continue,
                    Err(error) => return Err(error),
                }
                if !row.get::<_, bool>(8).map_err(cell_error)? {
                    return Err(LedgerError::Corrupt {
                        table: "schedule_claims",
                        column: "busy_holder_run_id",
                    });
                }
            }
            let schedule: String = row.get(0).map_err(cell_error)?;
            let occurrence = row.get(1).map_err(cell_error)?;
            let revision = row.get(2).map_err(cell_error)?;
            let outcome: String = row.get(4).map_err(cell_error)?;
            let busy_holder: Option<String> = row.get(5).map_err(cell_error)?;
            let at = row.get(6).map_err(cell_error)?;
            let schedule = schedule.parse().map_err(|_| LedgerError::Corrupt {
                table: "schedule_claims",
                column: "schedule_id",
            })?;
            let claim = typed(
                schedule,
                occurrence,
                revision,
                &run,
                &outcome,
                busy_holder.as_deref(),
                at,
            )?;
            claims.push(claim);
        }
        Ok(claims)
    }
}

fn read(
    conn: &rusqlite::Connection,
    schedule: ScheduleId,
    occurrence_ms: i64,
) -> Result<Option<SlotClaim>, LedgerError> {
    conn.query_row(
        "SELECT revision, run_id, outcome, busy_holder_run_id, claimed_at_ms FROM schedule_claims
          WHERE schedule_id = ?1 AND occurrence_ms = ?2",
        params![schedule.to_storage(), occurrence_ms],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )
    .optional()
    .map_err(|error| LedgerError::sql("read a slot", error))?
    .map(|(revision, run, outcome, busy_holder, claimed_at_ms)| {
        typed(
            schedule,
            occurrence_ms,
            revision,
            &run,
            &outcome,
            busy_holder.as_deref(),
            claimed_at_ms,
        )
    })
    .transpose()
}

fn validate_skip(outcome: SlotOutcome, busy_holder: Option<RunId>) -> Result<(), LedgerError> {
    if !matches!(
        outcome,
        SlotOutcome::SkippedMissed | SlotOutcome::SkippedBusy
    ) || (outcome == SlotOutcome::SkippedBusy) != busy_holder.is_some()
    {
        return Err(LedgerError::InvalidSkipOutcome);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn claim_skip_in_tx(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    schedule: ScheduleId,
    occurrence_ms: i64,
    revision: u32,
    planned_run: RunId,
    outcome: SlotOutcome,
    busy_holder: Option<RunId>,
) -> Result<SkippedSlot, LedgerError> {
    validate_skip(outcome, busy_holder)?;
    if let Some(existing) = read(tx, schedule, occurrence_ms)? {
        return match existing.state {
            SlotState::Settled(recorded)
                if recorded == outcome && existing.busy_holder == busy_holder =>
            {
                Ok(SkippedSlot { claim: existing })
            }
            SlotState::Settled(recorded) if recorded == outcome => {
                Err(LedgerError::SlotClaimConflict)
            }
            SlotState::Settled(recorded) => {
                Err(LedgerError::SlotOutcomeRecorded { outcome: recorded })
            }
            SlotState::Claimed => Err(LedgerError::SlotAlreadyClaimed {
                run: existing.run,
                revision: existing.revision,
            }),
        };
    }
    tx.execute(
        "INSERT INTO schedule_claims
           (schedule_id, occurrence_ms, revision, run_id, outcome, claimed_at_ms,
            busy_holder_run_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            schedule.to_storage(),
            occurrence_ms,
            revision,
            planned_run.to_storage(),
            outcome.token(),
            now,
            busy_holder.map(|run| run.to_storage()),
        ],
    )
    .map_err(|error| LedgerError::sql("record a skipped slot", error))?;
    Ok(SkippedSlot {
        claim: SlotClaim {
            schedule,
            occurrence_ms,
            revision,
            run: planned_run,
            state: SlotState::Settled(outcome),
            busy_holder,
            claimed_at_ms: now,
        },
    })
}

fn typed(
    schedule: ScheduleId,
    occurrence_ms: i64,
    revision: i64,
    run: &str,
    outcome: &str,
    busy_holder: Option<&str>,
    claimed_at_ms: i64,
) -> Result<SlotClaim, LedgerError> {
    let state = SlotState::parse(outcome)?;
    let busy_holder = busy_holder.map(parse_run).transpose()?;
    if matches!(state, SlotState::Settled(SlotOutcome::SkippedBusy)) != busy_holder.is_some() {
        return Err(LedgerError::Corrupt {
            table: "schedule_claims",
            column: "busy_holder_run_id",
        });
    }
    Ok(SlotClaim {
        schedule,
        occurrence_ms,
        revision: u32::try_from(revision).map_err(|_| LedgerError::Corrupt {
            table: "schedule_claims",
            column: "revision",
        })?,
        run: parse_run(run)?,
        state,
        busy_holder,
        claimed_at_ms,
    })
}

#[cfg(test)]
mod tests;
