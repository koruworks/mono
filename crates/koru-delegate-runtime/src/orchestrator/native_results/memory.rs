// SPDX-License-Identifier: MPL-2.0
//! Private Memory identity operations. No Store content or Memory creation runs here.
use super::*;

impl Orchestrator {
    /// Retained receipts need immutable subject proof, not a new effect lease.
    fn memory_subject(&mut self, run: RunId, device: DeviceId) -> Result<BotProvenance, RunError> {
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let provenance = admission.submission.provenance;
        if device.as_uuid().is_nil() || provenance.owner.device != device {
            return Err(refused("bot_owner_mismatch"));
        }
        let header = self.open(run)?.doc.document().header()?;
        if header.run != run || header.bot.as_ref() != Some(&provenance) {
            return Err(refused("bot_provenance_mismatch"));
        }
        Ok(provenance)
    }

    fn memory_fresh(
        &mut self,
        key: &NativeResultKey,
        config_revision: u64,
        policy_revision: u64,
        device: DeviceId,
    ) -> Result<(BotExecutionFence, FreshBotPolicyProof), RunError> {
        let provenance = self.memory_subject(key.run, device)?;
        let record = self.bot_record(
            &BotRunKey {
                owner: provenance.owner,
                bot: provenance.bot,
                run: key.run,
            },
            device,
        )?;
        if provenance.routine.is_some() {
            return Err(refused("bot_routine_native_not_ready"));
        }
        let fence = self
            .bot_admission_view(&record)?
            .fence
            .ok_or_else(|| refused("bot_fenced"))?;
        self.native_live_turn(&fence, device)?;
        if fence.attempt != key.attempt
            || fence.generation != key.generation
            || fence.turn != Some(key.turn)
        {
            return Err(refused("bot_fenced"));
        }
        let policy = self
            .native_policies
            .iter()
            .find(|p| p.key.owner == fence.owner && p.key.bot == fence.bot)
            .cloned()
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        if policy.config_revision != config_revision || policy.policy_revision != policy_revision {
            return Err(refused("bot_policy_stale"));
        }
        Ok((
            fence,
            FreshBotPolicyProof {
                global: policy,
                routine: None,
            },
        ))
    }

    pub fn read_memory_edit_allocation(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        fingerprint: [u8; 32],
        device: DeviceId,
    ) -> Result<Option<MemoryEditAllocation>, RunError> {
        let provenance = self.memory_subject(run, device)?;
        let allocation =
            self.native_ledger()?
                .memory_edit_allocation(run, operation, fingerprint)?;
        if allocation
            .as_ref()
            .is_some_and(|value| !value.subject.matches(&provenance))
        {
            return Err(refused("bot_provenance_mismatch"));
        }
        Ok(allocation)
    }

    pub fn allocate_memory_edit(
        &mut self,
        request: &AllocateMemoryEditRequest,
        device: DeviceId,
    ) -> Result<MemoryEditAllocation, RunError> {
        if !request.valid() {
            return Err(refused("bot_native_invalid"));
        }
        let provenance = self.memory_subject(request.key.run, device)?;
        if !request.subject.matches(&provenance) {
            return Err(refused("bot_provenance_mismatch"));
        }
        let intent = self
            .ports
            .store
            .intent(request.key.run)?
            .ok_or_else(|| refused("bot_native_invalid"))?;
        let state = self.open(request.key.run)?;
        if intent.state != koru_delegate_sqlite::IntentState::Satisfied
            || intent.run_document != Some(state.document)
            || intent.workspace != state.workspace
        {
            return Err(refused("bot_provenance_mismatch"));
        }
        let source = state.document;
        if let Some(old) = self.read_memory_edit_allocation(
            request.key.run,
            request.key.operation_id,
            request.caller_fingerprint,
            device,
        )? {
            if old.key != request.key
                || old.subject != request.subject
                || old.base != request.base
                || old.source_run_document != source
                || request.existing_fact_id.is_some_and(|id| id != old.fact_id)
            {
                return Err(koru_delegate_sqlite::BotLedgerError::Conflict.into());
            }
            return Ok(old);
        }
        let (fence, policy) = self.memory_fresh(
            &request.key,
            request.current_config_revision,
            request.current_policy_revision,
            device,
        )?;
        Ok(self
            .native_ledger()?
            .allocate_memory_edit(request, source, &fence, &policy)?)
    }

    pub fn resolve_memory_edit_base(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        resolution: &MemoryBaseResolution,
        config_revision: u64,
        policy_revision: u64,
        device: DeviceId,
    ) -> Result<MemoryEditAllocation, RunError> {
        let old = self
            .read_memory_edit_allocation(key.run, key.operation_id, fingerprint, device)?
            .ok_or_else(|| refused("bot_native_missing"))?;
        if old.key != *key {
            return Err(koru_delegate_sqlite::BotLedgerError::Conflict.into());
        }
        let MemoryBaseResolution::Resolved {
            authority,
            version,
            expected_revision,
        } = resolution
        else {
            return Err(refused("bot_native_invalid"));
        };
        let mut proposed = old.clone();
        proposed.resolution = resolution.clone();
        if !proposed.valid() {
            return Err(refused("bot_native_invalid"));
        }
        if let MemoryBaseResolution::Resolved {
            authority: prior,
            version: prior_version,
            expected_revision: prior_revision,
        } = &old.resolution
        {
            if prior != authority
                || prior_version != version
                || !same_native_edit_revision(prior_revision, expected_revision)
                    .map_err(|_| refused("bot_native_invalid"))?
            {
                return Err(koru_delegate_sqlite::BotLedgerError::Conflict.into());
            }
            return Ok(old); // Preserve the first encoding of the semantic VV.
        }
        let (fence, policy) = self.memory_fresh(key, config_revision, policy_revision, device)?;
        Ok(self.native_ledger()?.resolve_memory_edit_base(
            key,
            fingerprint,
            resolution,
            &fence,
            &policy,
        )?)
    }

    pub fn prepare_memory_edit(
        &mut self,
        request: &PrepareNativeResult,
        fingerprint: [u8; 32],
        authority: &MemoryEditAuthorityV1,
        device: DeviceId,
    ) -> Result<NativeResultIntentView, RunError> {
        let allocation = self
            .read_memory_edit_allocation(
                request.key.run,
                request.key.operation_id,
                fingerprint,
                device,
            )?
            .ok_or_else(|| refused("bot_native_missing"))?;
        if allocation.key != request.key {
            return Err(koru_delegate_sqlite::BotLedgerError::Conflict.into());
        }
        // The existing journal decoder verifies allocation/purpose binding.
        // Exact replay goes through the same body checks before reconciliation.
        if self.native_ledger()?.native_result(&request.key)?.is_some() {
            let old = self
                .native_ledger()?
                .prepare_memory_edit(request, fingerprint, authority)?;
            return self.reconcile_native_record(old);
        }
        let (fence, policy) = self.memory_fresh(
            &request.key,
            request.config_revision,
            request.policy_revision,
            device,
        )?;
        self.native_ledger()?
            .prepare_memory_edit(request, fingerprint, authority)?;
        let leased = self.native_ledger()?.lease_native_result(
            &request.key,
            request.request_fingerprint,
            &fence,
            &policy,
        )?;
        let mut view = leased.intent.view();
        if leased.execute.is_some() {
            view.disposition = NativeResultDisposition::Execute;
        }
        Ok(view)
    }
}
