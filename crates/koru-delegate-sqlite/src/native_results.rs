// SPDX-License-Identifier: MPL-2.0

//! Native effect reconciliation on the existing RunsDb writer. These records
//! are evidence, not canonical policy, execution authority or Run status.
use crate::BotLedgerError;
use koru_delegate_core::*;
use uuid::Uuid;

mod repository;
mod rows;
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
    /// A retained row never manufactures a fresh Execute disposition.
    pub fn view(&self) -> NativeResultIntentView {
        NativeResultIntentView {
            key: self.key.clone(),
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
pub struct NativeRecoveryCursor {
    pub created_at_ms: i64,
    pub run: RunId,
    pub operation_id: Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNativeResult {
    pub key: NativeResultKey,
    pub stage: NativeResultStage,
    pub lease_state: NativeLeaseState,
    pub cursor: NativeRecoveryCursor,
}

/// Borrowed typed inputs only. The trusted actor supplies fresh authority;
/// possession of an ID, snapshot or previous receipt grants none. No Store
/// callbacks, SQL closures, connection handles or request bodies cross this port.
pub trait NativeResultLedger {
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
        fresh: &BotPolicyBarrier,
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
        owner: BotOwner,
        bot: BotId,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError>;
}
