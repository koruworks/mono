// SPDX-License-Identifier: MPL-2.0

//! Exact retained SQL controls with in-memory document/session ports. No Store
//! is opened by these cold cleanup and authority recovery scenarios.
use super::*;

mod source;

fn waiting() -> (Harness, BotSubmission, BotExecutionFence) {
    waiting_with_memory(false)
}

fn waiting_with_memory(keep_memory: bool) -> (Harness, BotSubmission, BotExecutionFence) {
    let (h, submission, fence, _, _) = waiting_with(keep_memory, false, None);
    (h, submission, fence)
}

/// The waiting fixture, optionally with a native approval decision, plus the
/// exact fresh policy proof and delegate boot its later answer must present.
/// `effect`: before the decision, lease the fixture's native page effect and
/// leave it held (`Some(false)`) or confirm it DrainedUnknown (`Some(true)`),
/// through the real ledger methods only.
fn waiting_with(
    keep_memory: bool,
    native: bool,
    effect: Option<bool>,
) -> (
    Harness,
    BotSubmission,
    BotExecutionFence,
    FreshBotPolicyProof,
    DelegateBootId,
) {
    let (mut h, submission, fence, policy, request) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    h.agent(
        fence.run,
        AgentEvent::TurnStarted {
            turn_id: fence.turn.unwrap(),
        },
    );
    let header = h.orchestrator.header(fence.run).unwrap();
    let state = h.orchestrator.open(fence.run).unwrap();
    let base = DecisionPublicationBase {
        workspace: state.workspace,
        document: state.document,
        // The format the Bot Run was created at (BotRun5), read from the document.
        source_schema: u8::try_from(state.doc.document().unwrap().schema_version().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
        observed_created_at_ms: header.created_at_ms,
        timeline_len: state.doc.document().unwrap().timeline_len() as u64,
        subject_digest: decision_projection_subject_digest(&header).unwrap(),
        predecessor: DecisionProjectionPredecessor {
            observed_created_at_ms: header.created_at_ms,
            timeline_start: state.doc.document().unwrap().timeline_len() as u64,
            projection: None,
            status: header.status,
            failure_reason: header.failure_reason,
        },
        bookkeeping: None,
    };
    let ActorRef::Person { profile } = submission.request.requested_by else {
        panic!("person")
    };
    let mut db = h.schedule_store.inner.lock().unwrap();
    let retained = db.native_policy(&policy.key).unwrap().unwrap();
    let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
        version: DecisionVersionV1,
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        origin_attempt: fence.attempt,
        origin_generation: fence.generation,
        origin_session: fence.session,
        origin_turn: fence.turn.unwrap(),
        submission: submission.provenance.submission,
        routine: None,
        human_profile: profile,
        workspace: submission.workspace,
        target: DocumentId::new(),
        action: DecisionEditAction::EditDocument,
        operation_id: Uuid::new_v4(),
        edit_fingerprint: [3; 32],
        // A real semantic version vector: consume compares it semantically.
        expected_revision: DecisionBytes::new(vec![2, 1, 2, 2, 4]).unwrap(),
        payload_version: DecisionVersionV1,
        payload: DecisionBytes::new(b"private".to_vec()).unwrap(),
        captured_policy: submission.policy.clone(),
        global_policy: DecisionGlobalPolicy {
            key: policy.key.clone(),
            config_revision: DecisionRevision::new(policy.config_revision).unwrap(),
            policy_revision: DecisionRevision::new(policy.policy_revision).unwrap(),
            digest: DecisionDigest::from_bytes(retained.payload_digest),
        },
        routine_policy: None,
    });
    let binding = match (native, binding) {
        (true, DecisionBindingV1::KoruEdit(v)) => {
            DecisionBindingV1::NativeApproval(DecisionNativeBinding {
                version: v.version,
                owner: v.owner,
                bot: v.bot,
                run: v.run,
                origin_attempt: v.origin_attempt,
                origin_generation: v.origin_generation,
                origin_session: v.origin_session,
                origin_turn: v.origin_turn,
                submission: v.submission,
                routine: v.routine,
                human_profile: v.human_profile,
                workspace: v.workspace,
                prompt: ApprovalId::mint(),
                choices: DecisionList::new(vec![
                    DecisionNativeChoice {
                        id: ApprovalChoiceId::mint(),
                        intent: serde_json::from_str("\"allow\"").unwrap(),
                    },
                    DecisionNativeChoice {
                        id: ApprovalChoiceId::mint(),
                        intent: serde_json::from_str("\"reject\"").unwrap(),
                    },
                ])
                .unwrap(),
                native_deadline_at_ms: None,
                captured_policy: v.captured_policy,
                global_policy: v.global_policy,
                routine_policy: v.routine_policy,
            })
        }
        (_, binding) => binding,
    };
    let proof = FreshBotPolicyProof {
        global: policy,
        routine: None,
    };
    let boot = h.orchestrator.delegate_boot();
    if let Some(drained) = effect {
        db.prepare_native_result(&request).unwrap();
        let intent = db
            .lease_native_result(&request.key, request.request_fingerprint, &fence, &proof)
            .unwrap()
            .intent;
        if drained {
            let receipt = NativeEffectDrainReceipt {
                binding: intent.pending_effect().unwrap().binding,
                fenced_at: NativeEffectTimestamp::from_millis(999),
                drained_at: NativeEffectTimestamp::from_millis(1000),
            };
            db.confirm_native_result(&ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::DrainedUnknown { receipt },
            })
            .unwrap();
        }
    }
    db.prepare_bot_decision(&binding, &proof, boot, &base)
        .unwrap();
    drop(db);
    if keep_memory {
        use crate::orchestrator::publication::RunDocumentOwner;
        let state = h.orchestrator.runs.get_mut(&fence.run).unwrap();
        let RunDocumentOwner::Attached { handle, .. } =
            std::mem::replace(&mut state.doc, RunDocumentOwner::Unopened)
        else {
            panic!("attached")
        };
        state.doc = RunDocumentOwner::Attached {
            exclusive: true,
            handle: Box::new(ProjectionHandle {
                inner: handle,
                staging: koru_delegate_document::RunProjectionStaging::default(),
            }),
        };
    } else {
        // Simulate a cold actor without opening a replacement handle.
        h.orchestrator.runs.remove(&fence.run);
    }
    (h, submission, fence, proof, boot)
}

/// Real projection staging over the ordinary in-memory document fixture. This
/// exercises ownership/causal proofs without a Store or test-only proof token.
struct ProjectionHandle {
    inner: Box<dyn RunDocumentHandle>,
    staging: koru_delegate_document::RunProjectionStaging,
}
impl RunDocumentHandle for ProjectionHandle {
    fn document(&self) -> &RunDocument {
        self.inner.document()
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        self.inner.commit(write)
    }
    fn prepare_decision_projection(
        &mut self,
        intent: &koru_delegate_document::RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection> {
        self.inner.settle()?;
        self.staging
            .prepare(self.inner.document(), intent)
            .map_err(Into::into)
    }
    fn apply_prepared_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<koru_delegate_document::AppliedRunProjection> {
        self.staging.apply(self.inner.document(), prepared)?;
        self.inner.commit(&mut |_| Ok(()))?;
        self.inner.settle()?;
        if !self.inner.durable() {
            return Err(crate::publication_pending());
        }
        self.staging
            .acknowledge(
                self.inner.document(),
                prepared,
                &self.inner.document().version(),
            )
            .map_err(Into::into)
    }
    fn discard_prepared_projection(&mut self, transition: DecisionTransitionId) -> PortResult<()> {
        self.staging.discard(transition).map_err(Into::into)
    }
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    fn settle(&mut self) -> PortResult<()> {
        self.inner.settle()
    }
}

fn apply_next(h: &mut Harness, run: RunId) {
    let start = h.orchestrator.next_run_publication(run).unwrap().unwrap();
    let crate::RunPublicationDocument::Attached(mut handle) = start.document else {
        panic!("warm")
    };
    let prepared = match start.operation {
        crate::RunPublicationOperation::Prepare(intent) => {
            let candidate = handle.prepare_decision_projection(&intent).unwrap();
            h.orchestrator
                .record_run_preparation(start.ticket, &candidate)
                .unwrap()
        }
        crate::RunPublicationOperation::Apply(prepared) => prepared,
        crate::RunPublicationOperation::ObserveSource(_) => panic!("projection"),
    };
    let proof = handle.apply_prepared_projection(&prepared).unwrap();
    assert!(
        h.orchestrator.acknowledge_run_projection(&proof).is_err(),
        "durable bytes alone do not prove the worker returned its handle"
    );
    assert!(
        h.orchestrator
            .return_run_publication(start.ticket, handle)
            .is_ok()
    );
    h.orchestrator.acknowledge_run_projection(&proof).unwrap();
}

fn source_tool(turn: AgentTurnId, sequence: u64) -> SessionUpdate {
    SessionUpdate::Agent(SequencedAgentEvent {
        session_id: session(1),
        sequence: StreamSequence::new(sequence).unwrap(),
        event: AgentEvent::ToolCompleted {
            turn_id: turn,
            tool_call_id: koru_session_protocol::ToolCallId::mint(),
            succeeded: true,
            tool: None,
            failure: None,
            detail: None,
            output: None,
        },
    })
}

#[test]
fn decision_source_sql_acceptance_keeps_caches_until_applied_and_physical_return() {
    let (mut h, _, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(fence.run, epoch);
    let turn = fence.turn.unwrap();
    let before = h.orchestrator.runs[&fence.run].source_frontiers.clone();
    h.orchestrator
        .handle_from_producer(fence.run, epoch, source_tool(turn, 8))
        .unwrap();
    assert!(matches!(
        latest(&h, &fence).state,
        DecisionProjectionState::Pending
    ));
    assert_eq!(
        h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .turns[&turn]
            .tool_calls,
        0
    );
    assert_eq!(h.orchestrator.runs[&fence.run].source_frontiers, before);
    assert!(
        h.orchestrator
            .handle_from_producer(fence.run, epoch, source_tool(turn, 9))
            .is_err()
    );
    apply_next(&mut h, fence.run);
    assert_eq!(
        h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .turns[&turn]
            .tool_calls,
        1
    );
    assert_eq!(
        h.orchestrator.runs[&fence.run].source_frontiers.agent_after,
        Some(StreamSequence::new(8).unwrap())
    );
    assert!(h.orchestrator.runs[&fence.run].decision_source.is_none());
}

#[test]
fn decision_source_unknown_commit_reuses_exact_target_and_applies_tally_once() {
    let (mut h, _, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(fence.run, epoch);
    h.ledger.lock().unwrap().fail = Some("decision_source_after");
    h.orchestrator
        .handle_from_producer(fence.run, epoch, source_tool(fence.turn.unwrap(), 8))
        .unwrap();
    let retained = latest(&h, &fence);
    h.orchestrator.retry_decision_source(fence.run).unwrap();
    assert_eq!(latest(&h, &fence), retained);
    assert_eq!(
        h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .turns[&fence.turn.unwrap()]
            .tool_calls,
        0
    );
    apply_next(&mut h, fence.run);
    h.orchestrator.retry_decision_source(fence.run).unwrap();
    assert_eq!(
        h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .turns[&fence.turn.unwrap()]
            .tool_calls,
        1
    );
}

#[test]
fn decision_source_reconnect_discards_only_positively_missing_old_epoch_candidate() {
    let (mut h, _, fence) = waiting_with_memory(true);
    apply_next(&mut h, fence.run);
    let epoch = Uuid::new_v4();
    h.orchestrator.register_source_producer(fence.run, epoch);
    h.ledger.lock().unwrap().fail = Some("decision_source_before");
    assert!(
        h.orchestrator
            .handle_from_producer(fence.run, epoch, source_tool(fence.turn.unwrap(), 8))
            .is_err()
    );
    let latest_before = latest(&h, &fence);
    let replacement = Uuid::new_v4();
    h.orchestrator
        .register_source_producer(fence.run, replacement);
    assert!(h.orchestrator.retry_decision_source(fence.run).is_err());
    assert!(h.orchestrator.runs[&fence.run].decision_source.is_none());
    assert_eq!(latest(&h, &fence), latest_before);
    assert!(
        h.orchestrator
            .handle_from_producer(fence.run, epoch, source_tool(fence.turn.unwrap(), 8))
            .is_err()
    );
    h.orchestrator
        .handle_from_producer(fence.run, replacement, source_tool(fence.turn.unwrap(), 8))
        .unwrap();
    apply_next(&mut h, fence.run);
    assert_eq!(
        h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .turns[&fence.turn.unwrap()]
            .tool_calls,
        1
    );
}

fn latest(h: &Harness, fence: &BotExecutionFence) -> DecisionProjectionRecord {
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .latest_decision_projection(fence.owner, fence.run)
        .unwrap()
        .unwrap()
}

#[test]
fn decision_cold_header_restores_unopened_owner_without_opening_document() {
    let (mut h, _, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    assert!(
        h.orchestrator
            .header(fence.run)
            .unwrap_err()
            .publication_pending()
    );
    assert!(h.orchestrator.run_publication_pending(fence.run));
    assert!(h.orchestrator.runs[&fence.run].doc.document().is_err());
    assert!(h.orchestrator.runs[&fence.run].attempt.is_none());
    assert!(h.orchestrator.bot_budget_runs().contains(&fence.run));
}

#[test]
fn decision_cold_fresh_policy_refuses_before_provider_or_ordinary_open() {
    let (mut h, submission, _) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    let calls = h.sessions.lock().unwrap().calls.clone();
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&submission), 1, 1)
            .unwrap_err()
            .publication_pending()
    );
    assert_eq!(h.sessions.lock().unwrap().calls, calls);
}

#[test]
fn decision_stop_cold_retries_retained_cleanup_without_second_mutation() {
    let (mut h, submission, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .unwrap_err()
            .publication_pending()
    );
    let first = latest(&h, &fence);
    assert!(matches!(
        first.target,
        RunProjectionTargetV1::Control {
            action: DecisionControlAction::CleanupRequested {
                cause: DecisionClosureReason::Canceled,
                ..
            },
            ..
        }
    ));
    let creation = h.attempt(fence.run).provider_session.unwrap().idempotency;
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    h.orchestrator.runs.remove(&fence.run);
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .unwrap_err()
            .publication_pending()
    );
    assert_eq!(latest(&h, &fence), first);
    assert!(
        !h.attempt(fence.run).retired,
        "publication debt retains the admission"
    );
    assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
}

#[test]
fn decision_cleanup_failed_write_keeps_exact_retry_without_retiring_unfenced_consumer() {
    let (mut h, submission, fence) = waiting();
    h.ledger.lock().unwrap().fail = Some("decision_cleanup_before");
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    let transition = h.orchestrator.runs[&fence.run]
        .decision_cleanup
        .as_ref()
        .unwrap()
        .request
        .as_ref()
        .unwrap()
        .transition;
    let creation = h.attempt(fence.run).provider_session.unwrap().idempotency;
    assert!(
        !h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert_eq!(latest(&h, &fence).identity.transition, transition);
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
}

#[test]
fn decision_cleanup_lost_commit_reply_recovers_same_transition_after_restart() {
    let (mut h, submission, fence) = waiting();
    h.ledger.lock().unwrap().fail = Some("decision_cleanup_after");
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    let first = latest(&h, &fence);
    let creation = h.attempt(fence.run).provider_session.unwrap().idempotency;
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .contains(&Call::Retire(creation))
    );
    h.orchestrator.runs.remove(&fence.run);
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    assert_eq!(latest(&h, &fence), first);
    assert!(
        h.orchestrator.runs[&fence.run]
            .decision_cleanup
            .as_ref()
            .unwrap()
            .request
            .is_none()
    );
}

#[test]
fn decision_cold_restart_keeps_claim_without_ordinary_status_reconciliation() {
    let (mut h, _, fence) = waiting();
    h.documents.lock().unwrap().snapshots.clear();
    let report = h.orchestrator.restart().unwrap();
    assert!(report.unresolved.contains(&fence.run));
    assert!(report.claims.kept.contains(&(fence.run, fence.attempt)));
    assert!(h.orchestrator.run_publication_pending(fence.run));
    assert!(!h.attempt(fence.run).retired);
}

#[test]
fn decision_cleanup_deadline_read_failure_retains_retry_scheduling() {
    let (mut h, submission, fence) = waiting();
    assert!(
        h.orchestrator
            .bot_stop(&key(&submission), fence.owner.device, Some(&fence))
            .is_err()
    );
    h.ledger.lock().unwrap().fail = Some("bot_execution_read");
    assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
    assert!(h.ledger.lock().unwrap().fail.is_none());
    assert!(h.orchestrator.bot_budget_runs().contains(&fence.run));
}

#[test]
fn decision_bad_cold_owner_keeps_existing_claim_and_recovers_healthy_neighbor() {
    let (mut h, _, fence) = waiting();
    let old_scope = h
        .orchestrator
        .claims()
        .claims()
        .find(|claim| claim.attempt == fence.attempt)
        .unwrap()
        .scope
        .clone();
    let mut healthy = submission(2);
    healthy.provenance.bot = BotId::mint();
    // The preparation helper asserts that this new Run sends no input. Its
    // neighbor has already sent the turn whose decision we are recovering.
    h.sessions.lock().unwrap().calls.clear();
    let healthy_fence = prepared(&mut h, &healthy);
    running_session(
        &mut h,
        &healthy,
        CreationLookup::Known {
            session: summary(ManagedState::Idle, None),
        },
    );
    h.orchestrator.runs.remove(&healthy_fence.run);
    {
        let mut script = h.ledger.lock().unwrap();
        script.unavailable_decision_run = Some(fence.run);
        script.unavailable_admission_run = Some(fence.run);
    }
    let report = h.orchestrator.restart().unwrap();
    assert!(report.unresolved.contains(&fence.run));
    assert!(
        report
            .reattached
            .iter()
            .any(|started| started.run == healthy_fence.run)
    );
    assert!(report.claims.kept.contains(&(fence.run, fence.attempt)));
    assert_eq!(
        h.orchestrator
            .claims()
            .claims()
            .find(|claim| claim.attempt == fence.attempt)
            .unwrap()
            .scope,
        old_scope
    );
    assert!(
        h.orchestrator.ensure_scope_recovery().is_ok(),
        "an exact held scope is sufficient"
    );
}

#[test]
fn decision_unknown_scope_blocks_fresh_work_until_positive_reconciliation() {
    let (mut h, submission, fence) = waiting();
    h.orchestrator.claims.release(fence.attempt);
    {
        let mut script = h.ledger.lock().unwrap();
        script.unavailable_decision_run = Some(fence.run);
        script.unavailable_admission_run = Some(fence.run);
    }
    let report = h.orchestrator.restart().unwrap();
    assert!(report.unresolved.contains(&fence.run));
    assert!(!report.claims.kept.contains(&(fence.run, fence.attempt)));
    let calls = h.sessions.lock().unwrap().calls.clone();
    assert!(
        matches!(h.orchestrator.start(run_id(99), StartOrigin::Manual),
        Err(RunError::Port(error)) if error.code == "scope_recovery_pending")
    );
    assert!(matches!(h.orchestrator.resume(run_id(99), true),
        Err(RunError::Port(error)) if error.code == "scope_recovery_pending"));
    assert!(
        matches!(h.orchestrator.bot_dispatch(&key(&submission), fence.owner.device),
        Err(RunError::Port(error)) if error.code == "scope_recovery_pending")
    );
    assert_eq!(h.sessions.lock().unwrap().calls, calls);
    {
        let mut script = h.ledger.lock().unwrap();
        script.unavailable_decision_run = None;
        script.unavailable_admission_run = None;
    }
    let recovered = h.orchestrator.restart().unwrap();
    assert!(recovered.claims.kept.contains(&(fence.run, fence.attempt)));
    assert!(h.orchestrator.ensure_scope_recovery().is_ok());
}

#[test]
fn decision_bad_cold_owner_does_not_block_identity_page_neighbor_reservation_recovery() {
    let (mut h, _, fence) = waiting();
    let mut unused = submission(3);
    unused.provenance.bot = BotId::mint();
    h.orchestrator
        .bot_submit(unused.clone(), fence.owner.device)
        .unwrap();
    h.schedule_store
        .reserve_bot(unused.request.run, None)
        .unwrap();
    {
        let mut script = h.ledger.lock().unwrap();
        script.unavailable_decision_run = Some(fence.run);
        script.ops.clear();
    }
    h.orchestrator
        .recover_unused_bot_reservations(fence.owner.device)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_execution(unused.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Queued
    );
    assert!(
        !h.ledger
            .lock()
            .unwrap()
            .ops
            .contains(&"bot_execution_inventory_read")
    );
    assert!(!h.attempt(fence.run).retired);
}
