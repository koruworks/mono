// SPDX-License-Identifier: MPL-2.0

//! Bounded snapshots, never execution authority. SQL rechecks the exact current
//! composite proof and full Linked/current execution when acquiring a lease.
use super::*;

impl Orchestrator {
    /// The sole ledger decoder owns route/link/claim/intent agreement. This
    /// helper adds exact runtime admission equality, never a parallel decoder.
    pub(crate) fn linked_routine_admission(
        &mut self,
        admitted: &koru_delegate_sqlite::BotAdmissionRecord,
    ) -> Result<(), RunError> {
        let p = &admitted.submission.provenance;
        match (admitted.submission_version, p.routine) {
            (1, None) => Ok(()),
            (2, Some(subject)) => {
                let slot = RoutineSlot {
                    owner: p.owner,
                    bot: p.bot,
                    schedule: subject.schedule,
                    occurrence_ms: subject.occurrence_ms,
                };
                match self.bot_ledger()?.read_routine_claim(&slot)? {
                    RoutineClaimObservation::Claimed { record }
                        if record.valid()
                            && record.link_state == (RoutineLinkState::Linked {})
                            && record.submission_version == admitted.submission_version
                            && record.submission == admitted.submission =>
                    {
                        Ok(())
                    }
                    _ => Err(refused("bot_routine_link_required")),
                }
            }
            _ => Err(refused("bot_provenance_mismatch")),
        }
    }

    /// Current Routine authority is per Run. Global boot acknowledgement is a
    /// separate dispatch check; ordinary work never depends on this cache.
    pub(crate) fn bot_execution_policy(
        &mut self,
        run: RunId,
    ) -> Result<Option<FreshBotPolicyProof>, RunError> {
        if self.run_publication_pending(run) {
            return Err(crate::publication_pending().into());
        }
        let admitted = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        self.linked_routine_admission(&admitted)?;
        let p = &admitted.submission.provenance;
        if p.routine.is_none() {
            return Ok(None);
        }
        let current = self
            .native_ledger()?
            .current_native_policy(p.owner, p.bot)?
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        self.current_policy_for_submission(
            &admitted.submission,
            current.config_revision,
            current.policy_revision,
        )
        .map(Some)
    }
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
    /// those checks. Historical replay does not call this fresh-proof helper.
    pub(crate) fn native_policy_for_run(
        &mut self,
        key: &BotRunKey,
        config_revision: u64,
        policy_revision: u64,
    ) -> Result<FreshBotPolicyProof, RunError> {
        self.restore_publication_owner(key.run)?;
        let admitted = self
            .bot_ledger()?
            .bot_admission(key.run)?
            .ok_or(RunError::UnknownRun)?;
        self.linked_routine_admission(&admitted)?;
        let submission = admitted.submission;
        let provenance = &submission.provenance;
        if provenance.owner != key.owner
            || provenance.bot != key.bot
            || submission.request.run != key.run
        {
            return Err(refused("bot_provenance_mismatch"));
        }
        if self.run_publication_pending(key.run) {
            return Err(crate::publication_pending().into());
        }
        self.current_policy_for_submission(&submission, config_revision, policy_revision)
    }

    /// Process-local preclaim comparison. The host owns HTTP correlation; this
    /// owner derives all repeated identities from the complete preparation.
    /// No run admission is required and no execution permission is created.
    pub fn routine_policy_for_prepared(
        &mut self,
        prepared: &koru_delegate_core::RoutinePrepared,
        admission: &koru_delegate_core::BotAdmissionBarrier,
        offered: &FreshBotPolicyProof,
    ) -> Result<FreshBotPolicyProof, RunError> {
        let p = &prepared.submission.provenance;
        if !prepared.valid()
            || admission.owner != prepared.slot.owner
            || admission.bot != prepared.slot.bot
            || admission.paused
            || admission.epoch != p.config_revision
            || offered.global.config_revision != p.config_revision
            || offered.global.policy_revision != p.policy_revision
            || offered.routine.as_ref().map(|r| r.grant_revision)
                != p.routine.map(|r| r.grant_revision)
            || !offered.effects_allowed_for(&prepared.submission)
        {
            return Err(refused("bot_policy_stale"));
        }
        // The team first: paused, or not yet told in this delegate run. A
        // Bot that is itself paused keeps the refusal below.
        if let Some(code) = self.bot_held(p.owner, p.bot)?.team_refusal() {
            return Err(refused(code));
        }
        if self.bot_ledger()?.bot_barrier(p.owner, p.bot)?.as_ref() != Some(admission) {
            return Err(refused("bot_admission_stale"));
        }
        let current = self.current_policy_for_submission(
            &prepared.submission,
            p.config_revision,
            p.policy_revision,
        )?;
        if &current != offered {
            return Err(refused("bot_policy_stale"));
        }
        Ok(current)
    }

    // Shared bounded cache and durable-current composition. Native callers keep
    // their existing >= captured-revision semantics; preclaim checks equality.
    pub(crate) fn current_policy_for_submission(
        &mut self,
        submission: &koru_delegate_core::BotSubmission,
        config_revision: u64,
        policy_revision: u64,
    ) -> Result<FreshBotPolicyProof, RunError> {
        let provenance = &submission.provenance;
        let key = BotRunKey {
            owner: provenance.owner,
            bot: provenance.bot,
            run: submission.request.run,
        };
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
        if !proof.effects_allowed_for(submission) {
            return Err(refused("bot_policy_stale"));
        }
        Ok(proof)
    }
}
