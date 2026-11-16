// SPDX-License-Identifier: MPL-2.0

//! Both domains borrow the production ledger connection; never a second writer.
use super::SharedLedger;
use koru_delegate_core::decisions::*;
use koru_delegate_core::*;
use koru_delegate_sqlite::*;

impl BotLedger for SharedLedger {
    fn record_decision_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
        expected: &koru_delegate_core::decisions::DecisionWaitExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        outcome: koru_delegate_core::decisions::DecisionDeliveryOutcome,
        step: koru_delegate_core::TimelineEntryId,
        artifacts: u32,
        at_ms: i64,
    ) -> Result<
        (
            koru_delegate_core::EventId,
            koru_delegate_core::decisions::DecisionProjectionRecord,
        ),
        BotLedgerError,
    > {
        self.0.lock().unwrap().record_decision_delivery(
            receipt, expected, transition, outcome, step, artifacts, at_ms,
        )
    }
    fn decision_native_drain(
        &self,
        owner: BotOwner,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        observed_at_ms: i64,
    ) -> Result<koru_delegate_core::decisions::DecisionNativeDrain, BotLedgerError> {
        self.0.lock().unwrap().decision_native_drain(
            owner,
            run,
            attempt,
            generation,
            observed_at_ms,
        )
    }
    fn decision_retained_effect(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: koru_delegate_core::decisions::DecisionId,
    ) -> Result<Option<koru_delegate_core::decisions::DecisionRetainedEffect>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_retained_effect(owner, run, decision)
    }
    fn decision_other_effects_pending(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: koru_delegate_core::decisions::DecisionId,
    ) -> Result<bool, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_other_effects_pending(owner, run, decision)
    }
    fn bot_decision(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: koru_delegate_core::decisions::DecisionId,
    ) -> Result<Option<koru_delegate_sqlite::DecisionRecord>, BotLedgerError> {
        self.0.lock().unwrap().bot_decision(owner, run, decision)
    }
    fn decision_resume_runs(
        &self,
        after: Option<RunId>,
        limit: usize,
    ) -> Result<Vec<(BotOwner, RunId)>, BotLedgerError> {
        self.0.lock().unwrap().decision_resume_runs(after, limit)
    }
    fn decision_acknowledged_continuation(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::DecisionAcknowledgedContinuation>, BotLedgerError>
    {
        self.0
            .lock()
            .unwrap()
            .decision_acknowledged_continuation(owner, run)
    }
    fn read_bot_decision_wait(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: koru_delegate_core::decisions::BotWaitId,
        decision: koru_delegate_core::decisions::DecisionId,
    ) -> Result<Option<koru_delegate_core::decisions::BotDecisionWaitObservation>, BotLedgerError>
    {
        self.0
            .lock()
            .unwrap()
            .read_bot_decision_wait(owner, bot, run, wait, decision)
    }
    fn decision_cleanup_ownership(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::DecisionCleanupOwnership>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_cleanup_ownership(owner, run)
    }
    fn consume_decision_edit(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        request: &koru_delegate_core::PrepareNativeResult,
        payload: &[u8],
        fence: &koru_delegate_core::BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<koru_delegate_sqlite::NativeLeaseOutcome, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .consume_decision_edit(expected, transition, request, payload, fence, proof, boot)
    }
    fn settle_decision_effect(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        outcome: &koru_delegate_core::decisions::DecisionRetainedEffect,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .settle_decision_effect(expected, transition, outcome)
    }
    fn settle_decision_wait(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionWaitExpected,
        ack: &koru_delegate_core::decisions::DecisionProjectionAcknowledgment,
    ) -> Result<koru_delegate_sqlite::DecisionWaitRecord, BotLedgerError> {
        self.0.lock().unwrap().settle_decision_wait(expected, ack)
    }
    fn begin_bot_wait_cleanup(
        &mut self,
        fence: &koru_delegate_core::decisions::DecisionFence,
        reason: koru_delegate_core::decisions::BotWaitReason,
        observed: koru_delegate_core::StreamSequence,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        base: &koru_delegate_core::decisions::DecisionPublicationBase,
    ) -> Result<koru_delegate_sqlite::DecisionWaitRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .begin_bot_wait_cleanup(fence, reason, observed, transition, base)
    }
    fn record_nondecision_cleanup(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionWaitExpected,
        cleanup_projection: koru_delegate_core::decisions::DecisionTransitionId,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        cleanup: &koru_delegate_core::decisions::ConsumerCleanup,
    ) -> Result<koru_delegate_sqlite::DecisionWaitRecord, BotLedgerError> {
        self.0.lock().unwrap().record_nondecision_cleanup(
            expected,
            cleanup_projection,
            transition,
            cleanup,
        )
    }
    fn answer_bot_decision(
        &mut self,
        input: &koru_delegate_sqlite::AnswerDecision,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .answer_bot_decision(input, transition, proof, boot)
    }
    fn record_decision_native_answer(
        &mut self,
        owner: BotOwner,
        run: RunId,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        response: &koru_delegate_core::decisions::DecisionNativeResponse,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .record_decision_native_answer(owner, run, transition, response)
    }
    fn record_decision_consumer_cleanup(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionWaitExpected,
        cleanup_projection: koru_delegate_core::decisions::DecisionTransitionId,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        observation: &koru_delegate_core::decisions::ConsumerCleanup,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0.lock().unwrap().record_decision_consumer_cleanup(
            expected,
            cleanup_projection,
            transition,
            observation,
        )
    }
    fn enroll_decision_cleanup(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionWaitExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .enroll_decision_cleanup(expected, transition)
    }
    fn record_decision_enrollment(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        reply: &koru_delegate_core::decisions::BotWaitEnrollmentReply,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .record_decision_enrollment(expected, transition, reply)
    }
    fn record_decision_origin_stop(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        release: &koru_delegate_core::decisions::OriginWaitStop,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .record_decision_origin_stop(expected, transition, release)
    }
    fn reserve_decision_successor(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        host: koru_delegate_core::HostInstanceId,
        fingerprint: [u8; 32],
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0.lock().unwrap().reserve_decision_successor(
            expected,
            transition,
            host,
            fingerprint,
            proof,
            boot,
        )
    }
    fn bind_decision_successor(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        creation: koru_delegate_core::IdempotencyKey,
        host: koru_delegate_core::HostInstanceId,
        session: koru_delegate_core::SessionId,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .bind_decision_successor(expected, transition, creation, host, session)
    }
    fn record_decision_resume_input(
        &mut self,
        expected: &koru_delegate_core::decisions::DecisionExpected,
        transition: koru_delegate_core::decisions::DecisionTransitionId,
        input: koru_delegate_core::IdempotencyKey,
        fingerprint: [u8; 32],
        acknowledgment: Option<&koru_delegate_core::decisions::DecisionInputAcknowledgment>,
    ) -> Result<koru_delegate_sqlite::DecisionRecord, BotLedgerError> {
        self.0.lock().unwrap().record_decision_resume_input(
            expected,
            transition,
            input,
            fingerprint,
            acknowledgment,
        )
    }
    fn owned_bot_execution_page(
        &self,
        device: DeviceId,
        after: u64,
        through: Option<u64>,
    ) -> Result<BotBootPage, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .owned_bot_execution_page(device, after, through)
    }
    fn bot_execution_inventory(
        &self,
        device: DeviceId,
    ) -> Result<BotExecutionInventory, BotLedgerError> {
        self.0.lock().unwrap().bot_execution_inventory(device)
    }
    fn prepare_bot_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<DecisionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_bot_decision(binding, proof, boot, base)
    }
    fn request_decision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
        cause: DecisionClosureReason,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .request_decision_cleanup(expected, transition, cause)
    }
    fn decision_wait_execution(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionWaitExecution>, BotLedgerError> {
        self.0.lock().unwrap().decision_wait_execution(owner, run)
    }
    fn record_decision_source(
        &mut self,
        owner: BotOwner,
        transition: DecisionTransitionId,
        wait: BotWaitId,
        base: &DecisionPublicationBase,
        target: &RunProjectionTargetV1,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .record_decision_source(owner, transition, wait, base, target)
    }
    fn decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
        transition: DecisionTransitionId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_projection(owner, run, transition)
    }
    fn next_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        self.0.lock().unwrap().next_decision_projection(owner, run)
    }
    fn decision_projection_prefix(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Vec<DecisionProjectionRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .decision_projection_prefix(owner, run)
    }
    fn latest_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .latest_decision_projection(owner, run)
    }
    fn latest_applied_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .latest_applied_decision_projection(owner, run)
    }
    fn record_decision_preparation(
        &mut self,
        owner: BotOwner,
        prepared: &DecisionPreparedProjection,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .record_decision_preparation(owner, prepared)
    }
    fn acknowledge_decision_projection(
        &mut self,
        owner: BotOwner,
        acknowledgment: &DecisionProjectionAcknowledgment,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .acknowledge_decision_projection(owner, acknowledgment)
    }
    fn read_routine_claim(
        &self,
        slot: &RoutineSlot,
    ) -> Result<RoutineClaimObservation, BotLedgerError> {
        self.0.lock().unwrap().read_routine_claim(slot)
    }
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &RoutineLinkAcknowledgement,
    ) -> Result<RoutineClaimRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .acknowledge_routine_link(acknowledgement)
    }
    fn register_routine_route(
        &mut self,
        identity: &RoutineRouteIdentity,
    ) -> Result<RoutineRouteIdentity, BotLedgerError> {
        self.0.lock().unwrap().register_routine_route(identity)
    }
    fn stop_unstarted_routine(
        &mut self,
        key: &BotRunKey,
    ) -> Result<Option<RoutineClaimRecord>, BotLedgerError> {
        self.0.lock().unwrap().stop_unstarted_routine(key)
    }
    fn bot_boot_inventory(
        &self,
        owner: BotOwner,
        after: u64,
        through: Option<u64>,
    ) -> Result<BotBootPage, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .bot_boot_inventory(owner, after, through)
    }
    fn bot_has_unsettled(&self, owner: BotOwner, bot: BotId) -> Result<bool, BotLedgerError> {
        self.0.lock().unwrap().bot_has_unsettled(owner, bot)
    }
    fn recover_bot_reservation(&mut self, key: &BotRunKey) -> Result<bool, BotLedgerError> {
        self.0.lock().unwrap().recover_bot_reservation(key)
    }
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
    fn reserve_bot(
        &mut self,
        run: RunId,
        proof: Option<&koru_delegate_core::FreshBotPolicyProof>,
    ) -> Result<BotReservation, BotLedgerError> {
        self.0.lock().unwrap().reserve_bot(run, proof)
    }
    fn open_bot_attempt(
        &mut self,
        run: RunId,
        proof: Option<&koru_delegate_core::FreshBotPolicyProof>,
    ) -> Result<koru_delegate_sqlite::AttemptRow, BotLedgerError> {
        self.0.lock().unwrap().open_bot_attempt(run, proof)
    }
    fn activate_bot(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        proof: Option<&koru_delegate_core::FreshBotPolicyProof>,
    ) -> Result<(), BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .activate_bot(run, attempt, generation, proof)
    }
    fn set_bot_active(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        active: bool,
        proof: Option<&koru_delegate_core::FreshBotPolicyProof>,
    ) -> Result<(), BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .set_bot_active(run, attempt, generation, active, proof)
    }
    fn settle_bot(&mut self, run: RunId, outcome: BotSettlement) -> Result<(), BotLedgerError> {
        self.0.lock().unwrap().settle_bot(run, outcome)
    }
    fn prepare_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
        proof: Option<&koru_delegate_core::FreshBotPolicyProof>,
    ) -> Result<BotInitialSend, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_bot_initial_send(run, send, proof)
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
    fn memory_edit_allocation(
        &self,
        run: RunId,
        operation: uuid::Uuid,
        fingerprint: [u8; 32],
    ) -> Result<Option<MemoryEditAllocation>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .memory_edit_allocation(run, operation, fingerprint)
    }
    fn allocate_memory_edit(
        &mut self,
        request: &AllocateMemoryEditRequest,
        source: DocumentId,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .allocate_memory_edit(request, source, fence, fresh)
    }
    fn resolve_memory_edit_base(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        resolution: &MemoryBaseResolution,
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<MemoryEditAllocation, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .resolve_memory_edit_base(key, fingerprint, resolution, fence, fresh)
    }
    fn prepare_memory_edit(
        &mut self,
        request: &PrepareNativeResult,
        fingerprint: [u8; 32],
        authority: &MemoryEditAuthorityV1,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .prepare_memory_edit(request, fingerprint, authority)
    }
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
    fn install_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<koru_delegate_sqlite::RoutinePolicyRecord, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .install_routine_native_policy(barrier)
    }
    fn settle_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<koru_delegate_sqlite::RoutinePolicyRecord, BotLedgerError> {
        self.0.lock().unwrap().settle_routine_native_policy(barrier)
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
        fresh: &FreshBotPolicyProof,
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
        execution: &BotRunKey,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .created_native_page(execution, workspace, document)
    }
    fn pending_routine_native_effects(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .pending_routine_native_effects(owner, bot, schedule, after, limit)
    }
    fn current_routine_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
        schedule: ScheduleId,
    ) -> Result<Option<koru_delegate_sqlite::RoutinePolicyRecord>, BotLedgerError> {
        self.0
            .lock()
            .unwrap()
            .current_routine_native_policy(owner, bot, schedule)
    }
}

#[cfg(test)]
mod tests;
