// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_sqlite::NativeLeaseState;

impl Orchestrator {
    /// Internal discovery; an empty filtered page may still have a cursor.
    pub fn pending_routine_native_effects(
        &mut self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
        device: DeviceId,
    ) -> Result<NativePendingEffects, RunError> {
        if owner.device != device || device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        Ok(self
            .native_ledger()?
            .pending_routine_native_effects(owner, bot, schedule, after, limit)?)
    }
    pub fn native_outcomes_unknown(&mut self, run: RunId) -> Result<bool, RunError> {
        Ok(self.native_ledger()?.native_outcomes_unknown(run)?)
    }
    /// Discovery grants no permission to drain. The trusted caller supplies its
    /// locally vouched owner; every returned row validates retained ownership.
    pub fn pending_native_effects(
        &mut self,
        owner: BotOwner,
        bot: Option<BotId>,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
        device: DeviceId,
    ) -> Result<NativePendingEffects, RunError> {
        if owner.device != device
            || device.as_uuid().is_nil()
            || owner.user_scope.as_uuid().is_nil()
        {
            return Err(refused("bot_owner_mismatch"));
        }
        Ok(self
            .native_ledger()?
            .pending_native_effects(owner, bot, after, limit)?)
    }
    pub fn check_native_effect_drain(
        &mut self,
        key: &NativeResultKey,
        reason: &NativeDrainReason,
        device: DeviceId,
    ) -> Result<Option<NativePendingEffect>, RunError> {
        let record = self.native_record_for_device(key, device)?;
        if record.lease_state != NativeLeaseState::Held {
            return Ok(None);
        }
        let target = record.pending_effect()?;
        let header = self.header(key.run)?;
        let allowed = match reason {
            NativeDrainReason::GatewayStartup => true, // private process-start gate, never an AI restart/read
            NativeDrainReason::UnconfirmedOperation => {
                record.stage == NativeResultStage::Unconfirmed
            }
            NativeDrainReason::OwnerStop => header.status == RunStatus::Cancelled,
            NativeDrainReason::RunRecovery => {
                matches!(
                    header.status,
                    RunStatus::Interrupted { .. } | RunStatus::Failed { .. } | RunStatus::Cancelled
                ) || self.ports.store.current_attempt(key.run)?.is_some_and(|a| {
                    a.attempt == key.attempt && a.generation == key.generation && a.retired
                })
            }
            NativeDrainReason::TerminalTurn => self
                .open(key.run)?
                .attempt
                .as_ref()
                .is_some_and(|a| a.bot_terminal_turn == Some(key.turn)),
            NativeDrainReason::PolicyPending { key: policy_key } => {
                if policy_key.owner != target.owner || policy_key.bot != target.bot {
                    return Err(refused("bot_owner_mismatch"));
                }
                let current = self
                    .native_ledger()?
                    .current_native_policy(target.owner, target.bot)?;
                current.is_some_and(|p| {
                    p.key == *policy_key
                        && p.state == BotPolicyBarrierState::Pending
                        && record
                            .lease
                            .as_ref()
                            .is_some_and(|l| p.config_revision > l.policy.config_revision)
                })
            }
            NativeDrainReason::RoutinePolicyPending {
                owner,
                bot,
                schedule,
                request,
            } => {
                if *owner != target.owner || *bot != target.bot {
                    return Err(refused("bot_owner_mismatch"));
                }
                let companion = record
                    .lease
                    .as_ref()
                    .and_then(|l| l.routine.as_ref())
                    .ok_or_else(|| refused("bot_routine_lease_missing"))?;
                if companion.schedule != *schedule {
                    return Err(refused("bot_routine_subject_mismatch"));
                }
                self.native_ledger()?
                    .current_routine_native_policy(*owner, *bot, *schedule)?
                    .is_some_and(|p| {
                        p.request == *request
                            && p.state == BotPolicyBarrierState::Pending
                            && p.grant_revision > companion.grant_revision
                    })
            }
        };
        Ok(allowed.then_some(target))
    }
}
