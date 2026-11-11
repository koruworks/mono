// SPDX-License-Identifier: MPL-2.0

//! Both domains borrow the production ledger connection; never a second writer.
use super::SharedLedger;
use koru_delegate_core::*;
use koru_delegate_sqlite::*;

impl BotLedger for SharedLedger {
    fn active_bot_executions(
        &self,
        device: koru_delegate_core::DeviceId,
    ) -> Result<Vec<BotExecutionRecord>, BotLedgerError> {
        self.0.lock().unwrap().active_bot_executions(device)
    }
    fn admit_bot(
        &mut self,
        submission: &BotSubmission,
    ) -> Result<BotAdmissionRecord, BotLedgerError> {
        self.0.lock().unwrap().admit_bot(submission)
    }
    fn bot_admission(&self, run: RunId) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        self.0.lock().unwrap().bot_admission(run)
    }
    fn bot_execution(&self, run: RunId) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        self.0.lock().unwrap().bot_execution(run)
    }
    fn reconcile_bot_execution(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        self.0.lock().unwrap().reconcile_bot_execution(key)
    }
    fn reconcile_bot(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        self.0.lock().unwrap().reconcile_bot(key)
    }
    fn queued_bots(
        &self,
        after: u64,
        limit: u16,
    ) -> Result<Vec<QueuedBotAdmission>, BotLedgerError> {
        self.0.lock().unwrap().queued_bots(after, limit)
    }
    fn unsettled_bot(
        &self,
        owner: BotOwner,
        bot: BotId,
        limit: u16,
    ) -> Result<(Vec<BotExecutionRecord>, bool), BotLedgerError> {
        self.0.lock().unwrap().unsettled_bot(owner, bot, limit)
    }
    fn bot_barrier(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<BotAdmissionBarrier>, BotLedgerError> {
        self.0.lock().unwrap().bot_barrier(owner, bot)
    }
    fn apply_bot_barrier(
        &mut self,
        barrier: &BotAdmissionBarrier,
    ) -> Result<BotAdmissionBarrier, BotLedgerError> {
        self.0.lock().unwrap().apply_bot_barrier(barrier)
    }
    fn reserve_bot(&mut self, run: RunId) -> Result<BotReservation, BotLedgerError> {
        self.0.lock().unwrap().reserve_bot(run)
    }
    fn activate_bot(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
    ) -> Result<(), BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .activate_bot(run, attempt, generation)
    }
    fn set_bot_active(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        active: bool,
    ) -> Result<(), BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .set_bot_active(run, attempt, generation, active)
    }
    fn settle_bot(&mut self, run: RunId, outcome: BotSettlement) -> Result<(), BotLedgerError> {
        self.0.lock().unwrap().settle_bot(run, outcome)
    }
    fn prepare_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError> {
        self.0.lock().unwrap().prepare_bot_initial_send(run, send)
    }
    fn acknowledge_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .acknowledge_bot_initial_send(run, send)
    }
    fn record_bot_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
    ) -> Result<EventId, BotLedgerError> {
        self.0.lock().unwrap().record_bot_delivery(receipt)
    }
}
impl NativeResultLedger for SharedLedger {
    fn verified_native_result(
        &self,
        run: RunId,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        self.0.lock().unwrap().verified_native_result(run)
    }
    fn pending_native_effects(
        &self,
        owner: BotOwner,
        bot: Option<BotId>,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .pending_native_effects(owner, bot, after, limit)
    }
    fn native_outcomes_unknown(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.0.lock().unwrap().native_outcomes_unknown(run)
    }
    fn native_effects_pending(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.0.lock().unwrap().native_effects_pending(run)
    }
    fn install_native_policy(
        &mut self,
        barrier: &BotPolicyBarrier,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        self.0.lock().unwrap().install_native_policy(barrier)
    }
    fn native_policy(
        &self,
        key: &BotPolicyBarrierKey,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        self.0.lock().unwrap().native_policy(key)
    }
    fn current_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        self.0.lock().unwrap().current_native_policy(owner, bot)
    }
    fn settle_native_policy(
        &mut self,
        key: &BotPolicyBarrierKey,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        self.0.lock().unwrap().settle_native_policy(key)
    }
    fn prepare_native_result(
        &mut self,
        request: &PrepareNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.0.lock().unwrap().prepare_native_result(request)
    }
    fn lease_native_result(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        fence: &BotExecutionFence,
        fresh: &BotPolicyBarrier,
    ) -> Result<NativeLeaseOutcome, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .lease_native_result(key, fingerprint, fence, fresh)
    }
    fn native_result(
        &self,
        key: &NativeResultKey,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        self.0.lock().unwrap().native_result(key)
    }
    fn pending_native_results(
        &self,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<Vec<PendingNativeResult>, BotLedgerError> {
        self.0.lock().unwrap().pending_native_results(after, limit)
    }
    fn refuse_unleased_native_result(
        &mut self,
        key: &NativeResultKey,
        refusal: NativeResultRefusal,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .refuse_unleased_native_result(key, refusal)
    }
    fn confirm_native_result(
        &mut self,
        request: &ConfirmNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.0.lock().unwrap().confirm_native_result(request)
    }
    fn register_native_result(
        &mut self,
        key: &NativeResultKey,
        evidence: &NativeResultReference,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.0.lock().unwrap().register_native_result(key, evidence)
    }
    fn created_native_page(
        &self,
        owner: BotOwner,
        bot: BotId,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .created_native_page(owner, bot, workspace, document)
    }
}

#[cfg(test)]
mod tests;
