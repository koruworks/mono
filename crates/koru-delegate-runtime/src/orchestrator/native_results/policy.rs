// SPDX-License-Identifier: MPL-2.0

//! Bounded snapshots, never execution authority. SQL rechecks the exact current
//! composite proof and full Linked/current execution when acquiring a lease.
use super::*;

impl Orchestrator {
    pub fn synchronize_routine_policy(
        &mut self,
        barrier: RoutinePolicyBarrier,
        device: DeviceId,
    ) -> Result<BotPolicyBarrierState, RunError> {
        if barrier.owner.device != device || device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        self.native_ledger()?
            .install_routine_native_policy(&barrier)?;
        let record = self
            .native_ledger()?
            .settle_routine_native_policy(&barrier)?;
        // Install can return an immutable historical exact replay. It must not
        // replace a newer cache entry. Its retained receipt is still truthful;
        // fresh composition independently proves currentness below.
        if self.native_ledger()?.current_routine_native_policy(
            barrier.owner,
            barrier.bot,
            barrier.schedule,
        )? != Some(record.clone())
        {
            return Ok(record.state);
        }
        self.native_routine_policies.retain(|p| {
            p.owner != barrier.owner || p.bot != barrier.bot || p.schedule != barrier.schedule
        });
        if self.native_routine_policies.len() == POLICY_PROOFS {
            self.native_routine_policies.remove(0);
        }
        self.native_routine_policies.push(barrier);
        Ok(record.state)
    }

    /// Derive the subject from the canonical admission. A returned snapshot is
    /// not a lease, a live turn or a Linked proof; the caller and SQL still own
    /// those checks. Routine execution gates remain closed in this checkpoint.
    pub(crate) fn native_policy_for_run(
        &mut self,
        key: &BotRunKey,
        config_revision: u64,
        policy_revision: u64,
    ) -> Result<FreshBotPolicyProof, RunError> {
        let admitted = self
            .bot_ledger()?
            .bot_admission(key.run)?
            .ok_or(RunError::UnknownRun)?;
        let submission = admitted.submission;
        let provenance = &submission.provenance;
        if provenance.owner != key.owner
            || provenance.bot != key.bot
            || submission.request.run != key.run
        {
            return Err(refused("bot_provenance_mismatch"));
        }
        let cached = self
            .native_policies
            .iter()
            .find(|p| p.barrier.key.owner == key.owner && p.barrier.key.bot == key.bot)
            .cloned()
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        let global = cached.barrier;
        if global.config_revision != config_revision || global.policy_revision != policy_revision {
            return Err(refused("bot_policy_stale"));
        }
        let retained = self
            .native_ledger()?
            .native_policy(&global.key)?
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        // Retain the owner's validated digest in the same bounded entry. This
        // catches changed durable evidence without another hash implementation.
        if retained.key != global.key
            || retained.payload_digest != cached.digest
            || retained.config_revision != global.config_revision
            || retained.policy_revision != global.policy_revision
            || retained.effects_allowed != global.effects_allowed
            || retained.state != BotPolicyBarrierState::Effective
            || self
                .native_ledger()?
                .current_native_policy(key.owner, key.bot)?
                != Some(retained)
        {
            return Err(refused("bot_policy_stale"));
        }
        let routine = match provenance.routine {
            None => None,
            Some(subject) => {
                let barrier = self
                    .native_routine_policies
                    .iter()
                    .find(|p| {
                        p.owner == key.owner
                            && p.bot == key.bot
                            && p.schedule == subject.schedule
                            && p.grant_revision >= subject.grant_revision
                    })
                    .cloned()
                    .ok_or_else(|| refused("bot_policy_refresh_required"))?;
                let current = self
                    .native_ledger()?
                    .current_routine_native_policy(key.owner, key.bot, subject.schedule)?
                    .ok_or_else(|| refused("bot_policy_refresh_required"))?;
                if current.request != barrier.request
                    || current.grant_revision != barrier.grant_revision
                    || current.payload_digest
                        != barrier.digest().map_err(|_| refused("bot_policy_stale"))?
                    || current.effects_allowed != barrier.effects_allowed
                    || current.state != BotPolicyBarrierState::Effective
                {
                    return Err(refused("bot_policy_stale"));
                }
                Some(barrier)
            }
        };
        let proof = FreshBotPolicyProof { global, routine };
        if !proof.effects_allowed_for(&submission) {
            return Err(refused("bot_policy_stale"));
        }
        Ok(proof)
    }
}
