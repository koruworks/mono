// SPDX-License-Identifier: MPL-2.0

use super::*;

impl NativeResultLedger for RecordingStore {
    fn native_effects_pending(&self, run: RunId) -> Result<bool, BotLedgerError> {
        self.inner.lock().unwrap().native_effects_pending(run)
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
        self.inner.lock().unwrap().native_policy(key)
    }
    fn current_native_policy(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
        self.inner.lock().unwrap().current_native_policy(owner, bot)
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
        fresh: &BotPolicyBarrier,
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
        owner: BotOwner,
        bot: BotId,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<bool, BotLedgerError> {
        self.inner
            .lock()
            .unwrap()
            .created_native_page(owner, bot, workspace, document)
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
            request: BotRequestId::from_uuid(v4(90)),
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
    policy.key.request = BotRequestId::from_uuid(v4(92));
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
fn ended_prepared_intent_without_any_lease_can_be_definitively_refused() {
    let (mut h, _, fence, mut policy, request) = active();
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
