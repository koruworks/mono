// SPDX-License-Identifier: MPL-2.0

use super::{policy, rows::*, *};
use crate::BOT_PENDING_LIMIT;
use koru_delegate_core::*;

/// Check size without allocating a serialized copy of a malformed candidate.
fn bounded_json(value: &impl serde::Serialize) -> Result<String, BotLedgerError> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= 4 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("Routine payload too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count(0), value).map_err(|_| BotLedgerError::Invalid)?;
    serde_json::to_string(value).map_err(|_| BotLedgerError::Invalid)
}

fn skip(
    conn: &Connection,
    slot: RoutineSlot,
    revision: u32,
    run: RunId,
    outcome: RoutineSkipOutcome,
    now: i64,
) -> Result<RoutineSkippedClaim, BotLedgerError> {
    match observation(conn, &slot, now)? {
        // Retained truth wins, including Busy or an older revision. The caller
        // receives the actual typed outcome, never a fabricated Missed fact.
        RoutineClaimObservation::Skipped { record } => return Ok(record),
        RoutineClaimObservation::Claimed { .. } => return Err(BotLedgerError::Conflict),
        RoutineClaimObservation::Absent { .. } => {}
    }
    let record = RoutineSkippedClaim {
        slot,
        definition_revision: revision,
        planned_run: run,
        outcome,
        recorded_at_ms: now,
    };
    if !record.valid() {
        return Err(BotLedgerError::Invalid);
    }
    let (name, holder) = match outcome {
        RoutineSkipOutcome::SkippedMissed {} => ("skipped_missed", None),
        RoutineSkipOutcome::SkippedBusy { holder } => ("skipped_busy", Some(holder.to_storage())),
    };
    conn.execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,busy_holder_run_id,claimed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![slot.schedule.to_storage(),slot.occurrence_ms,revision,run.to_storage(),name,holder,now]).map_err(sql)?;
    // Re-read also proves this planned identity does not name an existing intent.
    match observation(conn, &slot, now)? {
        RoutineClaimObservation::Skipped { record } => Ok(record),
        _ => Err(bad("skip")),
    }
}

fn representative(
    conn: &Connection,
    facts: &RoutineClaimFacts,
    now: i64,
) -> Result<Option<RoutineSkippedClaim>, BotLedgerError> {
    facts
        .representative_miss()
        .map(|miss| {
            skip(
                conn,
                RoutineSlot {
                    occurrence_ms: miss.occurrence_ms,
                    ..facts.prepared().slot
                },
                facts.prepared().definition.revision,
                miss.planned_run,
                RoutineSkipOutcome::SkippedMissed {},
                now,
            )
        })
        .transpose()
}

impl RunsDb {
    /// Only the trusted runtime passes facts produced by consuming actual live
    /// document authority. No wire method accepts these process-local facts.
    /// A Held claim remains nonexecutable in this checkpoint, even after link.
    pub fn claim_bot_routine(
        &mut self,
        facts: RoutineClaimFacts,
        fresh: &FreshBotPolicyProof,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        self.write_with_error(WriteOp::ClaimBotRoutine,|tx,now| {
            let prepared = facts.prepared();
            let slot = &prepared.slot;
            let route = subject(tx,slot)?;
            if route.workspace != facts.workspace() || route.creation_request != facts.creation_request() { return Err(BotLedgerError::Fenced); }
            let old = observation(tx,slot,now)?;
            if !matches!(old,RoutineClaimObservation::Absent { .. }) {
                // Return retained winner before candidate policy, deadline,
                // serialization and capacity checks. Never append a new miss.
                let missed = facts.representative_miss().map(|miss| observation(tx,&RoutineSlot {occurrence_ms:miss.occurrence_ms,..*slot},now)).transpose()?;
                return Ok((old,match missed {Some(RoutineClaimObservation::Skipped {record})=>Some(record),_=>None}));
            }
            if now < slot.occurrence_ms { return Err(BotLedgerError::Fenced); }
            if now.checked_sub(slot.occurrence_ms).is_none_or(|elapsed|elapsed >= 60_000) {
                let record = skip(tx,*slot,prepared.definition.revision,prepared.ids.run,RoutineSkipOutcome::SkippedMissed {},now)?;
                return Ok((RoutineClaimObservation::Skipped {record},representative(tx,&facts,now)?));
            }
            policy::fresh(tx,&prepared.submission,fresh)?;
            // O85: the existing 50 pending entries are per owner/device/Bot;
            // any actual unsettled holder shadows that defensive queue check.
            // Host capacity two is still enforced only at reservation time.
            let holder: Option<RunId> = tx.query_row("SELECT CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND state IN ('queued','reserved','executing') ORDER BY CASE state WHEN 'executing' THEN 0 WHEN 'reserved' THEN 1 ELSE 2 END,queue_sequence LIMIT 1",
                params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage()],|r|Ok(parse_cell(r,0))).optional().map_err(sql)?.transpose()?;
            if let Some(holder) = holder {
                if holder == prepared.ids.run { return Err(BotLedgerError::Conflict); }
                let record = skip(tx,*slot,prepared.definition.revision,prepared.ids.run,RoutineSkipOutcome::SkippedBusy {holder},now)?;
                return Ok((RoutineClaimObservation::Skipped {record},representative(tx,&facts,now)?));
            }
            let pending: i64 = tx.query_row("SELECT count(*) FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND state='queued'",params![slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage()],|r|r.get(0)).map_err(sql)?;
            if pending >= BOT_PENDING_LIMIT as i64 { return Err(BotLedgerError::Capacity); }
            let payload = bounded_json(&prepared.submission)?;
            let ordinary = bounded_json(&prepared.submission.request)?;
            let p = &prepared.submission.provenance;
            let ids = &prepared.ids;
            let sequence: i64 = tx.query_row("SELECT COALESCE(MAX(queue_sequence),0)+1 FROM bot_admissions",[],|r|r.get(0)).map_err(sql)?;
            tx.execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'pending',?4,?4)",params![ids.run.to_storage(),route.workspace.to_storage(),ordinary,now]).map_err(sql)?;
            tx.execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,claimed_at_ms) VALUES(?1,?2,?3,?4,'claimed',?5)",params![slot.schedule.to_storage(),slot.occurrence_ms,prepared.definition.revision,ids.run.to_storage(),now]).map_err(sql)?;
            tx.execute("INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms,submission_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10,?10,2)",params![ids.run.to_storage(),sequence,p.owner.user_scope.to_storage(),p.owner.device.to_storage(),p.bot.to_storage(),ids.submission.to_storage(),p.config_revision,p.policy_revision,payload,now]).map_err(sql)?;
            tx.execute("INSERT INTO bot_routine_links(run_id,schedule_id,occurrence_ms,user_scope,device_id,bot_id,definition_revision,definition_digest,owner_handoff,grant_revision,preparation_hash,submission_request_id,chat_id,attempt_id,user_message_id,assistant_message_id,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'held',?17,?17)",
                params![ids.run.to_storage(),slot.schedule.to_storage(),slot.occurrence_ms,slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage(),prepared.definition.revision,prepared.definition.digest.as_bytes().as_slice(),prepared.definition.owner_handoff.map(|h|h.to_storage()),p.routine.as_ref().ok_or(BotLedgerError::Invalid)?.grant_revision,prepared.preparation_hash.as_bytes().as_slice(),ids.submission.to_storage(),ids.chat.to_string(),ids.attempt.to_string(),ids.user_message.to_string(),ids.assistant_message.to_string(),now]).map_err(sql)?;
            let missed = representative(tx,&facts,now)?;
            Ok((observation(tx,slot,now)?,missed))
        })
    }

    /// The outer private port authenticates current execution-owner access.
    /// Subject mismatch or corrupt retained evidence is never positive absence.
    pub fn read_routine_claim(
        &self,
        slot: &RoutineSlot,
    ) -> Result<RoutineClaimObservation, BotLedgerError> {
        observation(self.conn(), slot, self.now_ms())
    }

    pub fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &RoutineLinkAcknowledgement,
    ) -> Result<RoutineClaimRecord, BotLedgerError> {
        self.write_with_error(WriteOp::AcknowledgeRoutineLink,|tx,now| {
            let RoutineClaimObservation::Claimed {record} = observation(tx,&acknowledgement.claim.slot,now)? else { return Err(BotLedgerError::Conflict); };
            if !record.accepts(acknowledgement) { return Err(BotLedgerError::Conflict); }
            if record.link_state == acknowledgement.outcome.state() { return Ok(record); }
            match acknowledgement.outcome {
                RoutineLinkOutcome::Linked {} => {
                    tx.execute("UPDATE bot_routine_links SET state='linked',updated_at_ms=MAX(updated_at_ms,?2) WHERE run_id=?1 AND state='held'",params![record.claim.ids.run.to_storage(),now]).map_err(sql)?;
                }
                RoutineLinkOutcome::Terminal {reason} => close(tx,&record,reason,now)?,
            }
            match observation(tx,&record.claim.slot,now)? { RoutineClaimObservation::Claimed {record}=>Ok(record),_=>Err(bad("claim")) }
        })
    }
}

fn reason_name(reason: RoutineTerminalReason) -> &'static str {
    match reason {
        RoutineTerminalReason::Stopped => "stopped",
        RoutineTerminalReason::ConfigurationChanged => "configuration_changed",
        RoutineTerminalReason::AuthorityChanged => "authority_changed",
        RoutineTerminalReason::AlreadyTerminal => "already_terminal",
    }
}

/// Only unstarted queued work can close without the ordinary native/provider
/// retirement proof. Any attempt, reservation or unresolved effect refuses.
fn close(
    conn: &Connection,
    record: &RoutineClaimRecord,
    reason: RoutineTerminalReason,
    now: i64,
) -> Result<(), BotLedgerError> {
    let run = record.claim.ids.run;
    let safe: bool = conn.query_row("SELECT a.state='queued' AND a.active_since_ms IS NULL AND a.initial_send_key IS NULL AND a.active_elapsed_ms=0 AND i.state='pending' AND NOT EXISTS(SELECT 1 FROM run_attempts WHERE run_id=a.run_id) AND NOT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=a.run_id) FROM bot_admissions a JOIN run_intents i ON i.run_id=a.run_id WHERE a.run_id=?1",[run.to_storage()],|r|r.get(0)).map_err(sql)?;
    if !safe {
        return Err(BotLedgerError::Fenced);
    }
    conn.execute("UPDATE bot_routine_links SET state='terminal',terminal_reason=?2,updated_at_ms=MAX(updated_at_ms,?3) WHERE run_id=?1",params![run.to_storage(),reason_name(reason),now]).map_err(sql)?;
    conn.execute(
        "UPDATE bot_admissions SET state='settled',updated_at_ms=?2 WHERE run_id=?1",
        params![run.to_storage(), now],
    )
    .map_err(sql)?;
    conn.execute("UPDATE run_intents SET state='abandoned',updated_at_ms=?2 WHERE run_id=?1 AND state='pending'",params![run.to_storage(),now]).map_err(sql)?;
    Ok(())
}

pub(in crate::bots) fn stop(
    conn: &Connection,
    key: &BotRunKey,
    now: i64,
) -> Result<Option<RoutineClaimRecord>, BotLedgerError> {
    bounded_admission(conn, key.run)?;
    let Some(admission) = super::super::repository::read(conn, key.run)? else {
        return Err(BotLedgerError::Invalid);
    };
    if admission.submission.provenance.owner != key.owner
        || admission.submission.provenance.bot != key.bot
    {
        return Err(BotLedgerError::Fenced);
    }
    if admission.submission_version == 1 {
        return Ok(None);
    }
    let provenance = admission
        .submission
        .provenance
        .routine
        .ok_or_else(|| bad("routine"))?;
    let slot = RoutineSlot {
        owner: key.owner,
        bot: key.bot,
        schedule: provenance.schedule,
        occurrence_ms: provenance.occurrence_ms,
    };
    let RoutineClaimObservation::Claimed { record } = observation(conn, &slot, now)? else {
        return Err(bad("claim"));
    };
    if record.claim.ids.run != key.run {
        return Err(bad("run"));
    }
    if matches!(record.link_state, RoutineLinkState::Terminal { .. }) {
        return Ok(Some(record));
    }
    close(conn, &record, RoutineTerminalReason::Stopped, now)?;
    match observation(conn, &slot, now)? {
        RoutineClaimObservation::Claimed { record } => Ok(Some(record)),
        _ => Err(bad("claim")),
    }
}

pub(in crate::bots) fn settle_link(
    conn: &Connection,
    admission: &crate::BotAdmissionRecord,
    now: i64,
) -> Result<(), BotLedgerError> {
    if admission.submission_version == 1 {
        return Ok(());
    }
    let p = &admission.submission.provenance;
    let provenance = p.routine.ok_or_else(|| bad("routine"))?;
    let slot = RoutineSlot {
        owner: p.owner,
        bot: p.bot,
        schedule: provenance.schedule,
        occurrence_ms: provenance.occurrence_ms,
    };
    let RoutineClaimObservation::Claimed { record } = observation(conn, &slot, now)? else {
        return Err(bad("claim"));
    };
    if record.claim.ids.run != admission.submission.request.run {
        return Err(bad("run"));
    }
    if !matches!(record.link_state, RoutineLinkState::Terminal { .. }) {
        // The enclosing generic settlement owns attempt/retirement checks and
        // admission mutation. This only prevents a later Linked acknowledgement
        // from reopening its exact retained identity in that same transaction.
        conn.execute("UPDATE bot_routine_links SET state='terminal',terminal_reason='already_terminal',updated_at_ms=MAX(updated_at_ms,?2) WHERE run_id=?1",params![record.claim.ids.run.to_storage(),now]).map_err(sql)?;
    }
    Ok(())
}
