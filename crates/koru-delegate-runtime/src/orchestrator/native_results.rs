// SPDX-License-Identifier: MPL-2.0

//! Native writes are performed by the trusted semantic adapter. This owner
//! grants one durable effect lease and registers only its verified evidence.
use super::{Orchestrator, RunError, bots::refused};
use koru_delegate_core::*;
use koru_delegate_sqlite::{NativeIntentRecord, NativeResultLedger};

const POLICY_PROOFS: usize = 64;

impl Orchestrator {
    pub fn native_effects_pending(&mut self, run: RunId) -> Result<bool, RunError> {
        Ok(self.native_ledger()?.native_effects_pending(run)?)
    }

    fn native_ledger(&mut self) -> Result<&mut dyn NativeResultLedger, RunError> {
        self.ports
            .store
            .native_results()
            .ok_or_else(|| refused("bot_native_unavailable"))
    }

    /// The caller has authenticated a bearer, but supplies no owner identity.
    pub fn resolve_run_tool_authority(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        device: DeviceId,
    ) -> Result<RunToolAuthority, RunError> {
        let row = self
            .ports
            .store
            .current_attempt(run)?
            .ok_or(RunError::UnknownRun)?;
        if row.retired || row.phase.is_terminal() || row.generation != generation {
            return Err(refused("bot_fenced"));
        }
        let header = self.open(run)?.doc.document().header()?;
        if header.run != run
            || header.executing_device() != Some(device)
            || header.status.is_terminal()
        {
            return Err(refused("bot_fenced"));
        }
        let admission = self.bot_ledger()?.bot_admission(run)?;
        match (header.bot, admission) {
            (None, None) if !matches!(header.scope, WorkspaceScope::BotFolder { .. }) => {
                Ok(RunToolAuthority::Ordinary {})
            }
            (Some(provenance), Some(admission))
                if provenance == admission.submission.provenance =>
            {
                Ok(RunToolAuthority::Bot {
                    provenance,
                    policy: admission.submission.policy,
                })
            }
            _ => Err(refused("bot_provenance_mismatch")),
        }
    }

    /// Rebuild only from this process's current durable attempt. A missing
    /// projection refuses discovery until this owner explicitly republishes it.
    pub fn refresh_run_tool_authority(
        &mut self,
        run: RunId,
        device: DeviceId,
    ) -> Result<(), RunError> {
        let generation = self
            .ports
            .store
            .current_attempt(run)?
            .ok_or(RunError::UnknownRun)?
            .generation;
        let authority = self.resolve_run_tool_authority(run, generation, device)?;
        self.ports
            .mcp
            .publish_authority(run, generation, authority)?;
        Ok(())
    }

    pub fn synchronize_native_policy(
        &mut self,
        barrier: BotPolicyBarrier,
        device: DeviceId,
    ) -> Result<BotPolicyBarrierReceipt, RunError> {
        if barrier.key.owner.device != device || device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        self.native_ledger()?.install_native_policy(&barrier)?;
        let record = self.native_ledger()?.settle_native_policy(&barrier.key)?;
        self.native_policies
            .retain(|p| p.key.owner != barrier.key.owner || p.key.bot != barrier.key.bot);
        // Eviction only removes a proof: it cannot grant a lease. A later call
        // must supply a freshly synchronized AI snapshot again.
        if self.native_policies.len() == POLICY_PROOFS {
            self.native_policies.remove(0);
        }
        self.native_policies.push(barrier.clone());
        Ok(BotPolicyBarrierReceipt {
            barrier,
            state: record.state,
        })
    }

    pub fn reconcile_native_policy(
        &mut self,
        key: &BotPolicyBarrierKey,
        device: DeviceId,
    ) -> Result<BotPolicyBarrierReceipt, RunError> {
        if key.owner.device != device {
            return Err(refused("bot_owner_mismatch"));
        }
        let barrier = self
            .native_policies
            .iter()
            .find(|p| p.key == *key)
            .cloned()
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        let record = self.native_ledger()?.settle_native_policy(key)?;
        Ok(BotPolicyBarrierReceipt {
            barrier,
            state: record.state,
        })
    }

    pub fn native_effect_fence(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
    ) -> Result<CapturedBotPolicy, RunError> {
        self.native_live_turn(fence, device)?;
        let admission = self
            .bot_ledger()?
            .bot_admission(fence.run)?
            .ok_or(RunError::UnknownRun)?;
        Ok(admission.submission.policy)
    }

    fn native_live_turn(
        &mut self,
        fence: &BotExecutionFence,
        device: DeviceId,
    ) -> Result<(), RunError> {
        self.bot_live_fence(fence, device, true, true)?;
        let attempt = self.open(fence.run)?.attempt.as_ref().expect("validated");
        if fence.turn.is_none()
            || attempt.orchestrator_turn != fence.turn
            || attempt.bot_terminal_turn == fence.turn
        {
            return Err(refused("bot_turn_stale"));
        }
        Ok(())
    }

    pub fn prepare_native_effect(
        &mut self,
        request: &PrepareNativeResult,
        device: DeviceId,
    ) -> Result<NativeResultIntentView, RunError> {
        let RunToolAuthority::Bot { provenance, .. } =
            self.resolve_run_tool_authority(request.key.run, request.key.generation, device)?
        else {
            return Err(refused("bot_private_route_required"));
        };
        let record = self.bot_record(
            &BotRunKey {
                owner: provenance.owner,
                bot: provenance.bot,
                run: request.key.run,
            },
            device,
        )?;
        let fence = self
            .bot_admission_view(&record)?
            .fence
            .ok_or_else(|| refused("bot_fenced"))?;
        self.native_live_turn(&fence, device)?;
        if fence.attempt != request.key.attempt || fence.turn != Some(request.key.turn) {
            return Err(refused("bot_fenced"));
        }
        let policy = self
            .native_policies
            .iter()
            .find(|p| p.key.owner == fence.owner && p.key.bot == fence.bot)
            .cloned()
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        if policy.config_revision != request.config_revision
            || policy.policy_revision != request.policy_revision
        {
            return Err(refused("bot_policy_stale"));
        }
        self.native_ledger()?.prepare_native_result(request)?;
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

    fn native_record_for_device(
        &mut self,
        key: &NativeResultKey,
        device: DeviceId,
    ) -> Result<NativeIntentRecord, RunError> {
        let admission = self
            .bot_ledger()?
            .bot_admission(key.run)?
            .ok_or(RunError::UnknownRun)?;
        if admission.submission.provenance.owner.device != device || device.as_uuid().is_nil() {
            return Err(refused("bot_owner_mismatch"));
        }
        let header = self.open(key.run)?.doc.document().header()?;
        if header.run != key.run || header.bot.as_ref() != Some(&admission.submission.provenance) {
            return Err(refused("bot_provenance_mismatch"));
        }
        self.native_ledger()?
            .native_result(key)?
            .ok_or_else(|| refused("bot_native_missing"))
    }

    /// Recovery publishes already verified evidence even after cancellation.
    /// It never acquires another lease or repeats a Store effect.
    pub fn reconcile_native_result(
        &mut self,
        key: &NativeResultKey,
        device: DeviceId,
    ) -> Result<NativeResultIntentView, RunError> {
        let mut record = self.native_record_for_device(key, device)?;
        if record.stage == NativeResultStage::Prepared
            && record.lease_state == koru_delegate_sqlite::NativeLeaseState::None
        {
            let ended = self
                .ports
                .store
                .current_attempt(key.run)?
                .is_none_or(|row| row.retired || row.phase.is_terminal())
                || self.status(key.run)?.is_terminal();
            if ended {
                record = self
                    .native_ledger()?
                    .refuse_unleased_native_result(key, NativeResultRefusal::Unavailable)?;
            }
        }
        self.publish_native_result(record)
    }

    pub fn confirm_native_result(
        &mut self,
        request: &ConfirmNativeResult,
        device: DeviceId,
    ) -> Result<NativeResultIntentView, RunError> {
        self.native_record_for_device(&request.key, device)?;
        let record = self.native_ledger()?.confirm_native_result(request)?;
        self.publish_native_result(record)
    }

    fn publish_native_result(
        &mut self,
        record: NativeIntentRecord,
    ) -> Result<NativeResultIntentView, RunError> {
        if !matches!(
            record.stage,
            NativeResultStage::Verified | NativeResultStage::Registered
        ) {
            return Ok(record.view());
        }
        let evidence = record
            .verified
            .as_ref()
            .ok_or_else(|| refused("bot_native_invalid"))?;
        let state = self.open(record.key.run)?;
        state
            .doc
            .commit(&mut |document| document.register_native_result(evidence))?;
        state.doc.settle()?;
        if !state.doc.durable() {
            return Err(refused("bot_native_unconfirmed"));
        }
        if record.stage == NativeResultStage::Registered {
            return Ok(record.view());
        }
        Ok(self
            .native_ledger()?
            .register_native_result(&record.key, evidence)?
            .view())
    }
}
