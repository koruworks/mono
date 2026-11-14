// SPDX-License-Identifier: MPL-2.0

use super::{read as route, rows::*, *};
use koru_delegate_core::*;

/// Retained state uses the same request and pending/effective vocabulary as
/// global Bot policies. It does not acquire or drain native effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutinePolicyRecord {
    pub owner: BotOwner,
    pub bot: BotId,
    pub schedule: ScheduleId,
    pub request: NativePolicyRequestId,
    pub grant_revision: u64,
    pub payload_digest: RoutineDigest32,
    pub effects_allowed: bool,
    pub state: BotPolicyBarrierState,
}

fn held(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
) -> Result<bool, BotLedgerError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM native_routine_leases r JOIN native_result_intents n ON n.run_id=r.run_id AND n.operation_id=r.operation_id WHERE r.user_scope=?1 AND r.device_id=?2 AND r.bot_id=?3 AND r.schedule_id=?4 AND n.lease_state='held')",
        params![owner.user_scope.to_storage(), owner.device.to_storage(), bot.to_storage(), schedule.to_storage()], |r| r.get(0)).map_err(|e| sql(e).into())
}

fn read_policy(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
    request: Option<NativePolicyRequestId>,
) -> Result<Option<RoutinePolicyRecord>, BotLedgerError> {
    conn.query_row("SELECT CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END, grant_revision,
        CASE WHEN typeof(payload_digest)='blob' AND length(payload_digest)=32 THEN payload_digest END,
        effects_allowed, CASE WHEN state IN ('pending','effective') THEN state END
        FROM bot_routine_policy_barriers WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND schedule_id=?4
        AND (?5 IS NULL OR request_id=?5) ORDER BY grant_revision DESC LIMIT 1",
        params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage(),schedule.to_storage(),request.map(|r|r.to_string())], |r| Ok((|| {
            let grant_revision: u64 = r.get(1).map_err(sql)?;
            if grant_revision == 0 { return Err(bad("grant_revision")); }
            Ok(RoutinePolicyRecord {
                owner, bot, schedule, request: parse_cell(r,0)?, grant_revision,
                payload_digest: digest_cell(r,2)?,
                effects_allowed: match r.get::<_,i64>(3).map_err(sql)? { 0=>false, 1=>true, _=>return Err(bad("effects_allowed")) },
                state: match r.get::<_,String>(4).map_err(sql)?.as_str() { "pending"=>BotPolicyBarrierState::Pending, "effective"=>BotPolicyBarrierState::Effective, _=>return Err(bad("policy_state")) },
            })
        })())).optional().map_err(sql)?.transpose()
}

pub(super) fn current(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
) -> Result<Option<RoutinePolicyRecord>, BotLedgerError> {
    read_policy(conn, owner, bot, schedule, None)
}

impl RunsDb {
    pub fn install_routine_policy(
        &mut self,
        offered: &RoutinePolicyBarrier,
    ) -> Result<RoutinePolicyRecord, BotLedgerError> {
        let digest = offered.digest().map_err(|_| BotLedgerError::Invalid)?;
        self.write_with_error(WriteOp::InstallRoutinePolicy, |tx,now| {
            let registered = route(tx,offered.schedule)?.ok_or(BotLedgerError::Fenced)?;
            if registered.owner != offered.owner || registered.bot != offered.bot || registered.workspace != offered.policy.workspace { return Err(BotLedgerError::Fenced); }
            if let Some(old) = read_policy(tx,offered.owner,offered.bot,offered.schedule,Some(offered.request))? {
                return if old.payload_digest == digest { Ok(old) } else { Err(BotLedgerError::Conflict) };
            }
            if let Some(old) = current(tx,offered.owner,offered.bot,offered.schedule)? {
                if old.grant_revision == offered.grant_revision { return Err(BotLedgerError::Conflict); }
                if old.grant_revision > offered.grant_revision { return Err(BotLedgerError::Stale); }
            }
            let state = if held(tx,offered.owner,offered.bot,offered.schedule)? { "pending" } else { "effective" };
            tx.execute("INSERT INTO bot_routine_policy_barriers(user_scope,device_id,bot_id,schedule_id,request_id,grant_revision,payload_digest,effects_allowed,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![offered.owner.user_scope.to_storage(),offered.owner.device.to_storage(),offered.bot.to_storage(),offered.schedule.to_storage(),offered.request.to_string(),offered.grant_revision,digest.as_bytes().as_slice(),offered.effects_allowed,state,now]).map_err(sql)?;
            read_policy(tx,offered.owner,offered.bot,offered.schedule,Some(offered.request))?.ok_or_else(|| bad("policy"))
        })
    }
    pub fn current_routine_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<RoutinePolicyRecord>, BotLedgerError> {
        current(self.conn(), owner, bot, schedule)
    }
    pub fn routine_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        request: NativePolicyRequestId,
    ) -> Result<Option<RoutinePolicyRecord>, BotLedgerError> {
        read_policy(self.conn(), owner, bot, schedule, Some(request))
    }
    pub fn settle_routine_policy(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        request: NativePolicyRequestId,
    ) -> Result<RoutinePolicyRecord, BotLedgerError> {
        self.write_with_error(WriteOp::SettleRoutinePolicy, |tx,now| {
            let old = read_policy(tx,owner,bot,schedule,Some(request))?.ok_or(BotLedgerError::Invalid)?;
            if old.state == BotPolicyBarrierState::Pending && !held(tx,owner,bot,schedule)? {
                tx.execute("UPDATE bot_routine_policy_barriers SET state='effective',updated_at_ms=?6 WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND schedule_id=?4 AND request_id=?5",
                    params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage(),schedule.to_storage(),request.to_string(),now]).map_err(sql)?;
            }
            read_policy(tx,owner,bot,schedule,Some(request))?.ok_or_else(|| bad("policy"))
        })
    }
}

pub(super) fn fresh(
    conn: &Connection,
    submission: &BotSubmission,
    proof: &FreshBotPolicyProof,
) -> Result<(), BotLedgerError> {
    if !proof.effects_allowed_for(submission) {
        return Err(BotLedgerError::Fenced);
    }
    let p = &submission.provenance;
    // A new claim binds the current captured execution configuration. Native
    // lease freshness permits newer fences; it cannot authorize stale task
    // configuration at this earlier admission boundary.
    if proof.global.config_revision != p.config_revision {
        return Err(BotLedgerError::Stale);
    }
    if let Some(barrier) = super::super::repository::barrier(conn, p.owner, p.bot)? {
        if barrier.paused || p.config_revision < barrier.epoch {
            return Err(BotLedgerError::Fenced);
        }
    }
    let global = crate::native_results::current_policy(conn, p.owner, p.bot)?
        .ok_or(BotLedgerError::Fenced)?;
    if global.key != proof.global.key
        || global.payload_digest != crate::native_results::policy_digest(&proof.global)?
        || global.config_revision != proof.global.config_revision
        || global.policy_revision != proof.global.policy_revision
        || global.state != BotPolicyBarrierState::Effective
        || !global.effects_allowed
    {
        return Err(BotLedgerError::Fenced);
    }
    let offered = proof.routine.as_ref().ok_or(BotLedgerError::Fenced)?;
    let stored = current(conn, p.owner, p.bot, offered.schedule)?.ok_or(BotLedgerError::Fenced)?;
    if stored.request != offered.request
        || stored.grant_revision != offered.grant_revision
        || stored.payload_digest != offered.digest().map_err(|_| BotLedgerError::Invalid)?
        || stored.state != BotPolicyBarrierState::Effective
        || !stored.effects_allowed
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}
