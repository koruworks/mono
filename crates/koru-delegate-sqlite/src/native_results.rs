// SPDX-License-Identifier: MPL-2.0

//! Native effect reconciliation on the existing RunsDb writer. These records
//! are evidence, not canonical policy, execution authority or Run status.
use crate::BotLedgerError;
pub use koru_delegate_core::NativeRecoveryCursor;
use koru_delegate_core::*;
use uuid::Uuid;

mod drain;
mod memory;
mod repository;
mod routine;
mod rows;
pub(crate) use drain::held_routine;
pub(crate) use repository::policy_digest;
pub(crate) use rows::current_policy;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePolicyRecord {
    pub key: BotPolicyBarrierKey,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub payload_digest: [u8; 32],
    pub effects_allowed: bool,
    pub state: BotPolicyBarrierState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEffectLease {
    pub id: Uuid,
    pub policy: NativePolicyRecord,
    /// Immutable independent fence; present exactly for a leased Routine.
    pub routine: Option<crate::RoutinePolicyRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeLeaseState {
    None,
    Held,
    Settled,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NativeIntentRecord {
    pub key: NativeResultKey,
    pub purpose: NativeEffectPurpose,
    pub kind: NativeResultOperation,
    pub destination: WorkspaceId,
    pub target: DocumentId,
    pub store_request: Uuid,
    pub fingerprint: [u8; 32],
    pub expected_revision: Option<Vec<u8>>,
    pub requested_title: String,
    pub stage: NativeResultStage,
    pub lease: Option<NativeEffectLease>,
    pub lease_state: NativeLeaseState,
    pub verified: Option<NativeResultReference>,
    pub refusal: Option<NativeResultRefusal>,
    pub registered_at_ms: Option<i64>,
    pub drain: Option<NativeEffectDrainReceipt>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl std::fmt::Debug for NativeIntentRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeIntentRecord")
            .field("key", &self.key)
            .field("stage", &self.stage)
            .field("lease_state", &self.lease_state)
            .finish_non_exhaustive()
    }
}
impl NativeIntentRecord {
    pub fn pending_effect(&self) -> Result<NativePendingEffect, BotLedgerError> {
        let lease = self.lease.as_ref().ok_or(BotLedgerError::Fenced)?;
        Ok(NativePendingEffect {
            owner: lease.policy.key.owner,
            bot: lease.policy.key.bot,
            key: self.key.clone(),
            binding: self
                .view()
                .store_binding(lease.policy.key.owner, lease.policy.key.bot),
            cursor: NativeRecoveryCursor {
                created_at_ms: self.created_at_ms,
                run: self.key.run,
                operation_id: self.key.operation_id,
            },
        })
    }
    /// A retained row never manufactures a fresh Execute disposition.
    pub fn view(&self) -> NativeResultIntentView {
        NativeResultIntentView {
            key: self.key.clone(),
            purpose: self.purpose.clone(),
            kind: self.kind,
            destination: self.destination,
            request_fingerprint: self.fingerprint,
            target: self.target,
            store_request: self.store_request,
            stage: self.stage,
            verified: self.verified.clone(),
            disposition: match self.stage {
                NativeResultStage::Registered => NativeResultDisposition::AlreadyRegistered,
                NativeResultStage::Refused => NativeResultDisposition::Refused,
                _ => NativeResultDisposition::ReconcileOnly,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeLeaseOutcome {
    pub intent: NativeIntentRecord,
    /// Some exactly once, after the reservation commits. Lost acknowledgment
    /// leaves the retained row reconcilable, not another execution license.
    pub execute: Option<NativeEffectLease>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNativeResult {
    pub key: NativeResultKey,
    pub purpose: NativeEffectPurpose,
    pub stage: NativeResultStage,
    pub lease_state: NativeLeaseState,
    pub cursor: NativeRecoveryCursor,
}

/// Borrowed typed inputs only. The trusted actor supplies fresh authority;
/// possession of an ID, snapshot or previous receipt grants none. No Store
/// callbacks, SQL closures, connection handles or request bodies cross this port.
pub trait NativeResultLedger {
    fn memory_edit_allocation(
        &self,
        run: RunId,
        operation: Uuid,
        caller_fingerprint: [u8; 32],
    ) -> Result<Option<MemoryEditAllocation>, BotLedgerError>;
    fn allocate_memory_edit(
        &mut self,
        request: &AllocateMemoryEditRequest,
        source_run_document: DocumentId,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError>;
    fn resolve_memory_edit_base(
        &mut self,
        key: &NativeResultKey,
        caller_fingerprint: [u8; 32],
        resolution: &MemoryBaseResolution,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError>;
    fn prepare_memory_edit(
        &mut self,
        request: &PrepareNativeResult,
        caller_fingerprint: [u8; 32],
        authority: &MemoryEditAuthorityV1,
    ) -> Result<NativeIntentRecord, BotLedgerError>;
    fn pending_native_effects(
        &self,
        owner: BotOwner,
        bot: Option<BotId>,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError>;
    /// Subject-filtered discovery only, never permission to acquire or drain.
    fn pending_routine_native_effects(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError>;
    fn current_routine_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<crate::RoutinePolicyRecord>, BotLedgerError>;
    fn install_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<crate::RoutinePolicyRecord, BotLedgerError>;
    fn settle_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<crate::RoutinePolicyRecord, BotLedgerError>;
    fn native_outcomes_unknown(&self, run: RunId) -> Result<bool, BotLedgerError>;
    /// Held effects may still commit; verified evidence still needs publication.
    /// Neither condition permits releasing the Run's execution capacity.
    fn native_effects_pending(&self, run: RunId) -> Result<bool, BotLedgerError>;
    fn install_native_policy(
        &mut self,
        barrier: &BotPolicyBarrier,
    ) -> Result<NativePolicyRecord, BotLedgerError>;
    fn native_policy(
        &self,
        key: &BotPolicyBarrierKey,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError>;
    fn current_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError>;
    fn settle_native_policy(
        &mut self,
        key: &BotPolicyBarrierKey,
    ) -> Result<NativePolicyRecord, BotLedgerError>;
    fn prepare_native_result(
        &mut self,
        request: &PrepareNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError>;
    fn lease_native_result(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<NativeLeaseOutcome, BotLedgerError>;
    fn native_result(
        &self,
        key: &NativeResultKey,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError>;
    fn pending_native_results(
        &self,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<Vec<PendingNativeResult>, BotLedgerError>;
    /// One exact Run's retained proof awaiting canonical registration, by PK.
    fn verified_native_result(
        &self,
        run: RunId,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError>;
    fn refuse_unleased_native_result(
        &mut self,
        key: &NativeResultKey,
        refusal: NativeResultRefusal,
    ) -> Result<NativeIntentRecord, BotLedgerError>;
    fn confirm_native_result(
        &mut self,
        request: &ConfirmNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError>;
    fn register_native_result(
        &mut self,
        key: &NativeResultKey,
        evidence: &NativeResultReference,
    ) -> Result<NativeIntentRecord, BotLedgerError>;
    fn created_native_page(
        &self,
        execution: &BotRunKey,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError>;
}
