// SPDX-License-Identifier: MPL-2.0

use super::*;
mod drain;
mod interrupt;
mod memory;
mod policy;
mod settle;

impl NativeResultLedger for RecordingStore {
    fn memory_edit_allocation(
        &self,
        run: RunId,
        operation: uuid::Uuid,
        fingerprint: [u8; 32],
    ) -> Result<Option<MemoryEditAllocation>, BotLedgerError> {
        self.inner
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
        self.inner
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
        self.inner.lock().unwrap().resolve_memory_edit_base(
            key,
            fingerprint,
            resolution,
            fence,
            fresh,
        )
    }
    fn prepare_memory_edit(
        &mut self,
        request: &PrepareNativeResult,
        fingerprint: [u8; 32],
        authority: &MemoryEditAuthorityV1,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .prepare_memory_edit(request, fingerprint, authority)
    }
    fn verified_native_result(
        &self,
        run: RunId,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        self.inner.lock().unwrap().verified_native_result(run)
    }
    fn pending_native_effects(
        &self,
        owner: BotOwner,
        bot: Option<BotId>,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<NativePendingEffects, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .pending_native_effects(owner, bot, after, limit)
    }
    fn native_outcomes_unknown(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.inner.lock().unwrap().native_outcomes_unknown(run)
    }
    fn native_effects_pending(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.inner.lock().unwrap().native_effects_pending(run)
    }

    fn install_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<koru_delegate_sqlite::RoutinePolicyRecord, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .install_routine_native_policy(barrier)
    }
    fn settle_routine_native_policy(
        &mut self,
        barrier: &RoutinePolicyBarrier,
    ) -> Result<koru_delegate_sqlite::RoutinePolicyRecord, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .settle_routine_native_policy(barrier)
    }
    fn install_native_policy(
        &mut self,
        barrier: &BotPolicyBarrier,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        self.inner.lock().unwrap().install_native_policy(barrier)
    }
    fn native_policy(
        &self,
        key: &BotPolicyBarrierKey,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        let mut row = self.inner.lock().unwrap().native_policy(key)?;
        if self.script.lock().unwrap().corrupt_native_policy_digest {
            if let Some(row) = row.as_mut() {
                row.payload_digest[0] ^= 1;
            }
        }
        Ok(row)
    }
    fn current_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        self.check("bot_current_policy")?;
        let mut row = self
            .inner
            .lock()
            .unwrap()
            .current_native_policy(owner, bot)?;
        if self.script.lock().unwrap().corrupt_native_policy_digest {
            if let Some(row) = row.as_mut() {
                row.payload_digest[0] ^= 1;
            }
        }
        Ok(row)
    }
    fn settle_native_policy(
        &mut self,
        key: &BotPolicyBarrierKey,
    ) -> Result<NativePolicyRecord, BotLedgerError> {
        self.inner.lock().unwrap().settle_native_policy(key)
    }
    fn prepare_native_result(
        &mut self,
        request: &PrepareNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.inner.lock().unwrap().prepare_native_result(request)
    }
    fn lease_native_result(
        &mut self,
        key: &NativeResultKey,
        fingerprint: [u8; 32],
        fence: &BotExecutionFence,
        fresh: &FreshBotPolicyProof,
    ) -> Result<NativeLeaseOutcome, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .lease_native_result(key, fingerprint, fence, fresh)
    }
    fn native_result(
        &self,
        key: &NativeResultKey,
    ) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
        self.inner.lock().unwrap().native_result(key)
    }
    fn pending_native_results(
        &self,
        after: Option<&NativeRecoveryCursor>,
        limit: u16,
    ) -> Result<Vec<PendingNativeResult>, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .pending_native_results(after, limit)
    }
    fn refuse_unleased_native_result(
        &mut self,
        key: &NativeResultKey,
        refusal: NativeResultRefusal,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .refuse_unleased_native_result(key, refusal)
    }
    fn confirm_native_result(
        &mut self,
        request: &ConfirmNativeResult,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.inner.lock().unwrap().confirm_native_result(request)
    }
    fn register_native_result(
        &mut self,
        key: &NativeResultKey,
        evidence: &NativeResultReference,
    ) -> Result<NativeIntentRecord, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .register_native_result(key, evidence)
    }
    fn created_native_page(
        &self,
        execution: &BotRunKey,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError> {
        self.inner
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
        self.inner
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
        self.check("bot_current_routine_policy")?;
        self.inner
            .lock()
            .unwrap()
            .current_routine_native_policy(owner, bot, schedule)
    }
}

fn active() -> (
    Harness,
    BotSubmission,
    BotExecutionFence,
    BotPolicyBarrier,
    PrepareNativeResult,
) {
    let mut h = bot_harness();
    let s = submission(1);
    let mut fence = prepared(&mut h, &s);
    observe(&mut h, &fence);
    fence.turn = Some(
        h.orchestrator
            .bot_begin_turn(&fence, fence.owner.device, s.input.clone())
            .unwrap(),
    );
    let policy = BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: fence.owner,
            bot: fence.bot,
            request: BotRequestId::from_uuid(v4(90)).into(),
        },
        config_revision: 1,
        policy_revision: 1,
        policy: s.policy.clone(),
        effects_allowed: true,
    };
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run: fence.run,
            attempt: fence.attempt,
            generation: fence.generation,
            turn: fence.turn.unwrap(),
            operation_id: v4(91),
        },
        kind: NativeResultOperation::CreatePage,
        destination: s.workspace,
        target: None,
        request_fingerprint: [4; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "Result".into(),
    };
    (h, s, fence, policy, request)
}

#[test]
fn routine_native_reason_cannot_drain_interactive_composite_none_lease() {
    let (mut h, _, fence, policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    let intent = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert_eq!(intent.disposition, NativeResultDisposition::Execute);
    let schedule = ScheduleId::from_uuid(uuid::Uuid::new_v4());
    let page = h
        .orchestrator
        .pending_routine_native_effects(fence.owner, fence.bot, schedule, None, 1, device)
        .unwrap();
    assert!(page.targets.is_empty());
    assert!(page.next.is_some());
    assert!(
        h.orchestrator
            .check_native_effect_drain(
                &request.key,
                &NativeDrainReason::RoutinePolicyPending {
                    owner: fence.owner,
                    bot: fence.bot,
                    schedule,
                    request: BotRequestId::mint().into()
                },
                device
            )
            .is_err()
    );
    assert!(
        h.orchestrator
            .check_native_effect_drain(&request.key, &NativeDrainReason::GatewayStartup, device)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        h.orchestrator
            .prepare_native_effect(&request, device)
            .unwrap()
            .disposition,
        NativeResultDisposition::ReconcileOnly
    );
}

#[test]
fn authority_is_derived_and_unknown_stale_or_mismatched_state_never_becomes_ordinary() {
    let (mut h, s, fence, _, _) = active();
    let authority = h
        .orchestrator
        .resolve_run_tool_authority(fence.run, fence.generation, fence.owner.device)
        .unwrap();
    assert!(
        matches!(authority, RunToolAuthority::Bot { provenance, policy } if provenance == s.provenance && policy == s.policy)
    );
    assert!(
        h.orchestrator
            .resolve_run_tool_authority(RunId::mint(), fence.generation, fence.owner.device)
            .is_err()
    );
    assert!(
        h.orchestrator
            .resolve_run_tool_authority(
                fence.run,
                AttemptGeneration::FIRST.next(),
                fence.owner.device
            )
            .is_err()
    );
    assert!(
        h.orchestrator
            .resolve_run_tool_authority(fence.run, fence.generation, DeviceId::new())
            .is_err()
    );
}

#[test]
fn native_effect_requires_fresh_policy_and_issues_one_lease_across_retry_and_barrier() {
    let (mut h, _, fence, mut policy, mut request) = active();
    let device = fence.owner.device;
    // Boot replay now supplies the initial policy. Exercise loss of that
    // separate bounded native payload proof through its real eviction path.
    for _ in 0..64 {
        let mut other = policy.clone();
        other.key.bot = BotId::mint();
        h.orchestrator
            .synchronize_native_policy(other, device)
            .unwrap();
    }
    assert!(
        h.orchestrator
            .prepare_native_effect(&request, device)
            .is_err()
    );
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy.clone(), device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    let first = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert_eq!(first.disposition, NativeResultDisposition::Execute);
    let replay = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert_eq!(replay.target, first.target);
    assert_eq!(replay.disposition, NativeResultDisposition::ReconcileOnly);
    policy.key.request = BotRequestId::from_uuid(v4(92)).into();
    policy.config_revision = 2;
    policy.policy_revision = 2;
    policy.effects_allowed = false;
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy.clone(), device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    assert!(
        h.orchestrator
            .prepare_native_effect(&request, device)
            .is_err()
    );
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::Unconfirmed,
            },
            device,
        )
        .unwrap();
    assert_eq!(
        h.orchestrator
            .reconcile_native_policy(&policy.key, device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::NotApplied {
                    code: NativeResultRefusal::AccessDenied,
                },
            },
            device,
        )
        .unwrap();
    assert_eq!(
        h.orchestrator
            .reconcile_native_policy(&policy.key, device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    request.key.operation_id = v4(93);
    request.config_revision = 2;
    request.policy_revision = 2;
    assert!(
        h.orchestrator
            .prepare_native_effect(&request, device)
            .is_err()
    );
}

#[test]
fn verified_native_result_is_registered_once_and_partial_result_survives_terminal_run() {
    let (mut h, _, fence, policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    let intent = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    let evidence = NativeResultReference {
        operation_id: request.key.operation_id,
        workspace: request.destination,
        document: intent.target,
        version: vec![1, 2],
        title: "Saved".into(),
        confirmed_at_ms: 1,
        kind: NativeResultKind::CreatedPage,
    };
    // A terminal Run prevents new execution, but not publishing an existing durable effect.
    let pending = h
        .orchestrator
        .bot_stop(
            &BotRunKey {
                owner: fence.owner,
                bot: fence.bot,
                run: fence.run,
            },
            device,
            Some(&fence),
        )
        .unwrap_err();
    assert!(matches!(pending,RunError::Port(error) if error.code == "bot_native_effects_pending"));
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
    let row = h
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(fence.run)
        .unwrap()
        .unwrap();
    assert!(
        !row.retired,
        "Stop cannot release a held native effect's capacity"
    );
    let confirm = ConfirmNativeResult {
        key: request.key.clone(),
        request_fingerprint: request.request_fingerprint,
        observation: NativeResultObservation::Durable {
            evidence: evidence.clone(),
        },
    };
    assert_eq!(
        h.orchestrator
            .confirm_native_result(&confirm, device)
            .unwrap()
            .stage,
        NativeResultStage::Registered
    );
    assert_eq!(
        h.orchestrator
            .reconcile_native_result(&request.key, device)
            .unwrap()
            .disposition,
        NativeResultDisposition::AlreadyRegistered
    );
    let header = h
        .orchestrator
        .open(fence.run)
        .unwrap()
        .doc
        .document()
        .header()
        .unwrap();
    assert_eq!(header.native_results, vec![evidence]);
    assert!(header.artifacts.is_empty());
    assert!(!h.orchestrator.native_effects_pending(fence.run).unwrap());
    h.orchestrator.cancel(fence.run).unwrap();
}

#[test]
fn completed_native_turn_refuses_new_effects_but_preserves_held_reconciliation() {
    let (mut h, _, fence, policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    h.agent(
        fence.run,
        AgentEvent::TurnCompleted {
            turn_id: fence.turn.unwrap(),
        },
    );
    assert!(h.orchestrator.native_effect_fence(&fence, device).is_err());
    let mut late = request.clone();
    late.key.operation_id = v4(199);
    assert!(h.orchestrator.prepare_native_effect(&late, device).is_err());
    assert!(
        h.orchestrator
            .ports_mut()
            .store
            .native_results()
            .unwrap()
            .native_result(&late.key)
            .unwrap()
            .is_none()
    );
    let retained = h
        .orchestrator
        .reconcile_native_result(&request.key, device)
        .unwrap();
    assert_eq!(retained.disposition, NativeResultDisposition::ReconcileOnly);
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
}

#[test]
fn failed_run_publication_keeps_verified_pending_and_recovery_never_leases_again() {
    let (mut h, _, fence, policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy, device)
        .unwrap();
    let intent = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    let evidence = NativeResultReference {
        operation_id: request.key.operation_id,
        workspace: request.destination,
        document: intent.target,
        version: vec![1],
        title: "Saved".into(),
        confirmed_at_ms: 1,
        kind: NativeResultKind::CreatedPage,
    };
    h.documents.lock().unwrap().fail_commit = true;
    assert!(
        h.orchestrator
            .confirm_native_result(
                &ConfirmNativeResult {
                    key: request.key.clone(),
                    request_fingerprint: request.request_fingerprint,
                    observation: NativeResultObservation::Durable {
                        evidence: evidence.clone()
                    }
                },
                device
            )
            .is_err()
    );
    let retained = h
        .orchestrator
        .ports_mut()
        .store
        .native_results()
        .unwrap()
        .native_result(&request.key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.stage, NativeResultStage::Verified);
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
    assert_eq!(
        h.orchestrator
            .prepare_native_effect(&request, device)
            .unwrap()
            .disposition,
        NativeResultDisposition::ReconcileOnly
    );
    h.documents.lock().unwrap().fail_commit = false;
    assert_eq!(
        h.orchestrator
            .reconcile_native_result(&request.key, device)
            .unwrap()
            .stage,
        NativeResultStage::Registered
    );
    assert_eq!(
        h.orchestrator
            .open(fence.run)
            .unwrap()
            .doc
            .document()
            .header()
            .unwrap()
            .native_results,
        vec![evidence]
    );
}

#[test]
fn restart_cannot_reconstruct_current_policy_from_retained_digest() {
    let (mut h, _, fence, policy, request) = active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, fence.owner.device)
        .unwrap();
    let mut h = restarted(h, (host(1), CreationLookup::NoEvidence));
    let error = h
        .orchestrator
        .reconcile_native_policy(&policy.key, fence.owner.device)
        .unwrap_err();
    assert!(matches!(error,RunError::Port(error) if error.code=="bot_policy_refresh_required"));
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
    // Re-supplying the exact AI snapshot does not clear an uncertain lease.
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    assert!(h.orchestrator.native_effects_pending(fence.run).unwrap());
}

#[test]
fn boot_policy_pending_is_not_ready_until_native_lease_settles() {
    let (mut h, s, fence, mut policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy.clone(), device)
        .unwrap();
    h.orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    policy.config_revision = 2;
    policy.key.request = BotRequestId::from_uuid(v4(92)).into();
    policy.effects_allowed = false;
    let barrier = BotAdmissionBarrier {
        owner: fence.owner,
        bot: fence.bot,
        request: BotRequestId::mint(),
        epoch: 2,
        paused: true,
    };
    let boot = h
        .orchestrator
        .bot_hello(fence.owner, BotBootScan::Probe {}, device)
        .unwrap()
        .boot;
    let receipt = h
        .orchestrator
        .synchronize_boot_bot(boot, barrier.clone(), policy.clone(), device)
        .unwrap();
    assert_eq!(receipt.state, BotBootState::PendingPolicy);
    let page = h
        .orchestrator
        .bot_hello(fence.owner, BotBootScan::Start {}, device)
        .unwrap();
    assert_eq!(
        page.entries[0].identity.run, s.request.run,
        "executing owners need fresh replay too"
    );
    h.orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key,
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::NotApplied {
                    code: NativeResultRefusal::AccessDenied,
                },
            },
            device,
        )
        .unwrap();
    assert_eq!(
        h.orchestrator
            .synchronize_boot_bot(boot, barrier, policy, device)
            .unwrap()
            .state,
        BotBootState::Ready
    );
    let hello = h
        .orchestrator
        .bot_hello(fence.owner, BotBootScan::Start {}, device)
        .unwrap();
    assert_eq!(hello.entries.len(), 1);
    assert!(
        matches!(&hello.entries[0].admission, BotBootAdmission::Present { barrier } if barrier.epoch == 2 && barrier.paused)
    );
}

#[test]
fn ended_prepared_intent_without_any_lease_can_be_definitively_refused() {
    let (mut h, _, fence, mut policy, mut request) = active();
    policy.config_revision = 2;
    request.config_revision = 2;
    policy.key.request = BotRequestId::from_uuid(v4(92)).into();
    policy.effects_allowed = false;
    h.orchestrator
        .synchronize_native_policy(policy, fence.owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .prepare_native_effect(&request, fence.owner.device)
            .is_err()
    );
    assert!(!h.orchestrator.native_effects_pending(fence.run).unwrap());
    h.orchestrator.cancel(fence.run).unwrap();
    assert_eq!(
        h.orchestrator
            .reconcile_native_result(&request.key, fence.owner.device)
            .unwrap()
            .stage,
        NativeResultStage::Refused
    );
}
