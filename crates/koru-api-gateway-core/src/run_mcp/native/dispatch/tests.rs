// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{Locale, Timestamp};
use koru_store_server::memory::MemoryStore;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Policy {
    evidence: NativePolicyEvidence,
    wakes: AtomicUsize,
}
impl PolicySource for Policy {
    fn current(&self, request: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        assert_eq!(request.provenance.owner, self.evidence.barrier.key.owner);
        Ok(self.evidence.clone())
    }
    fn settled(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct State {
    intent: Option<NativeResultIntentView>,
    held: bool,
    leases: usize,
    prepares: usize,
    syncs: usize,
    confirms: usize,
}
struct Owner {
    fence: BotExecutionFence,
    policy: CapturedBotPolicy,
    state: Mutex<State>,
    pending: AtomicBool,
    lose_confirm: AtomicBool,
}
impl BotDelegatePort for Owner {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut state = self.state.lock().unwrap();
        Ok(match command {
            BotCommand::Reconcile { key } => BotReply::Admission {
                admission: BotAdmissionView {
                    key,
                    run: self.fence.run,
                    state: BotAdmissionState::Executing,
                    queue_sequence: 1,
                    fence: Some(self.fence.clone()),
                },
            },
            BotCommand::ReconcileNativeResult { key } => match &state.intent {
                Some(intent) => {
                    assert_eq!(intent.key, key);
                    BotReply::NativeResult {
                        intent: intent.clone(),
                    }
                }
                None => BotReply::Missing,
            },
            BotCommand::SynchronizePolicy { barrier } => {
                state.syncs += 1;
                BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: if self.pending.load(Ordering::SeqCst) {
                            BotPolicyBarrierState::Pending
                        } else {
                            BotPolicyBarrierState::Effective
                        },
                    },
                }
            }
            BotCommand::EffectFence { fence } => {
                assert_eq!(fence, self.fence);
                BotReply::EffectFence {
                    fence,
                    policy: self.policy.clone(),
                }
            }
            BotCommand::PrepareNativeResult { request } => {
                state.prepares += 1;
                if state.intent.is_none() {
                    state.intent = Some(NativeResultIntentView {
                        purpose: NativeEffectPurpose::Result {},
                        key: request.key,
                        kind: request.kind,
                        destination: request.destination,
                        request_fingerprint: request.request_fingerprint,
                        target: request
                            .target
                            .unwrap_or_else(koru_store_core::DocumentId::new),
                        store_request: Uuid::new_v4(),
                        stage: NativeResultStage::Prepared,
                        verified: None,
                        disposition: NativeResultDisposition::ReconcileOnly,
                    });
                }
                let execute = !state.held;
                if execute {
                    state.held = true;
                    state.leases += 1;
                }
                let mut intent = state.intent.clone().unwrap();
                intent.disposition = if execute {
                    NativeResultDisposition::Execute
                } else {
                    NativeResultDisposition::ReconcileOnly
                };
                BotReply::NativeResult { intent }
            }
            BotCommand::ConfirmNativeResult { request } => {
                state.confirms += 1;
                let intent = state.intent.as_mut().unwrap();
                assert_eq!(intent.key, request.key);
                assert_eq!(intent.request_fingerprint, request.request_fingerprint);
                match request.observation {
                    NativeResultObservation::DrainedUnknown { .. } => {
                        intent.stage = NativeResultStage::SettledUnknown;
                        intent.disposition = NativeResultDisposition::ReconcileOnly;
                    }
                    NativeResultObservation::Durable { evidence } => {
                        intent.verified = Some(evidence);
                        intent.stage = NativeResultStage::Verified;
                        if self.lose_confirm.swap(false, Ordering::SeqCst) {
                            return Ok(BotReply::Missing);
                        }
                        intent.stage = NativeResultStage::Registered;
                        intent.disposition = NativeResultDisposition::AlreadyRegistered;
                    }
                    NativeResultObservation::Unconfirmed => {
                        intent.stage = NativeResultStage::Unconfirmed
                    }
                    NativeResultObservation::NotApplied { .. } => {
                        intent.stage = NativeResultStage::Refused
                    }
                }
                BotReply::NativeResult {
                    intent: intent.clone(),
                }
            }
            _ => panic!("unexpected native command"),
        })
    }
}
struct Fixture {
    memory: MemoryStore,
    record: RunAuthorityRecord,
    provenance: BotProvenance,
    policy: Policy,
    owner: Owner,
}
impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("native-gateway-test").unwrap();
        let (scope, profile, workspace) = memory
            .with_store(|store| {
                let profile = store
                    .create_profile("Test", None, None, Locale::default(), Timestamp::now())
                    .unwrap()
                    .id;
                let workspace = store
                    .create_workspace(profile, "Work", Timestamp::now())
                    .unwrap()
                    .id;
                (store.local_user_scope().unwrap(), profile, workspace)
            })
            .unwrap();
        let owner = BotOwner {
            user_scope: scope,
            device: DeviceId::new(),
        };
        let bot = BotId::mint();
        let run = RunId::mint();
        let captured = CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
        };
        let provenance = BotProvenance {
            owner,
            bot,
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        };
        let fence = BotExecutionFence {
            owner,
            bot,
            run,
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            session: SessionId::mint(),
            turn: Some(AgentTurnId::mint()),
        };
        Self {
            memory,
            record: RunAuthorityRecord {
                run,
                workspace,
                generation: AttemptGeneration::FIRST,
                grants: [workspace].into(),
                run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
                    if cfg!(windows) {
                        "D:/native-test"
                    } else {
                        "/native-test"
                    },
                ))
                .unwrap(),
                state: koru_artifact_core::AuthorityState::Active,
                created_at_ms: 0,
                revoked_at_ms: None,
            },
            provenance,
            policy: Policy {
                evidence: NativePolicyEvidence {
                    profile,
                    barrier: BotPolicyBarrier {
                        key: BotPolicyBarrierKey {
                            owner,
                            bot,
                            request: Uuid::new_v4().try_into().unwrap(),
                        },
                        config_revision: 1,
                        policy_revision: 1,
                        policy: captured.clone(),
                        effects_allowed: true,
                    },
                },
                wakes: AtomicUsize::new(0),
            },
            owner: Owner {
                fence,
                policy: captured,
                state: Mutex::new(State::default()),
                pending: AtomicBool::new(false),
                lose_confirm: AtomicBool::new(false),
            },
        }
    }
    fn authority(&self) -> NativeStoreAuthority {
        NativeStoreAuthority {
            owner_scope: self.provenance.owner.user_scope,
            expected_profile: self.policy.evidence.profile,
            workspace: self.record.workspace,
        }
    }
    fn arguments(&self) -> Value {
        json!({"operation_id":Uuid::new_v4(),"workspace_id":self.record.workspace,"title":"Native result"})
    }
    fn call(&self, tool: &str, arguments: Value) -> Result<Value, ToolFailure> {
        invoke(
            &self.memory.client(),
            &self.policy,
            &self.owner,
            &self.record,
            self.provenance.clone(),
            self.owner.policy.clone(),
            tool,
            arguments,
        )
    }
    fn retain_prepared(&self, tool: &str, mut arguments: Value, held: bool) -> PreparedNativeWrite {
        let operation_id = operation(&mut arguments).unwrap();
        let prepared = koru_ai_server::native_store_backend(self.memory.client(), self.authority())
            .prepare_native_write(tool, arguments)
            .unwrap();
        let d = prepared.description();
        let mut state = self.owner.state.lock().unwrap();
        state.held = held;
        state.intent = Some(NativeResultIntentView {
            purpose: NativeEffectPurpose::Result {},
            key: NativeResultKey {
                run: self.record.run,
                attempt: self.owner.fence.attempt,
                generation: self.record.generation,
                turn: self.owner.fence.turn.unwrap(),
                operation_id,
            },
            kind: match d.kind {
                NativeWriteKind::Created => NativeResultOperation::CreatePage,
                NativeWriteKind::Edited => NativeResultOperation::EditPage,
            },
            destination: d.workspace,
            request_fingerprint: fingerprint(&d.fingerprint).unwrap(),
            target: d.document.unwrap_or_else(koru_store_core::DocumentId::new),
            store_request: Uuid::new_v4(),
            stage: NativeResultStage::Prepared,
            verified: None,
            disposition: NativeResultDisposition::ReconcileOnly,
        });
        prepared
    }
}

#[test]
fn native_create_registers_and_exact_retry_never_executes_again() {
    let f = Fixture::new();
    let arguments = f.arguments();
    let saved = f.call("create_document", arguments.clone()).unwrap();
    assert_eq!(saved["status"], "saved");
    assert_eq!(f.call("create_document", arguments).unwrap(), saved);
    let state = f.owner.state.lock().unwrap();
    assert_eq!((state.leases, state.prepares, state.confirms), (1, 1, 1));
    let workspace = f.record.workspace;
    assert_eq!(
        f.memory
            .with_store(move |store| store.documents(workspace).unwrap().len())
            .unwrap(),
        1
    );
}

#[test]
fn ordinary_native_write_rejects_retained_memory_purpose_before_store_effect() {
    let f = Fixture::new();
    let arguments = f.arguments();
    f.retain_prepared("create_document", arguments.clone(), false);
    {
        let mut state = f.owner.state.lock().unwrap();
        let intent = state.intent.as_mut().unwrap();
        intent.purpose = NativeEffectPurpose::Memory {
            authority: MemoryEditAuthorityV1 {
                version: 1,
                owner: f.provenance.owner,
                bot: f.provenance.bot,
                submission: f.provenance.submission,
                captured_config_revision: f.provenance.config_revision,
                creation_request: BotRequestId::mint(),
                workspace: intent.destination,
                memory_document: intent.target,
                action: MemoryEditAction::Remember,
            },
        };
    }
    assert_eq!(
        f.call("create_document", arguments).unwrap_err().code,
        "native_operation_conflict"
    );
    let state = f.owner.state.lock().unwrap();
    assert_eq!((state.leases, state.prepares, state.confirms), (0, 0, 0));
    let workspace = f.record.workspace;
    assert!(
        f.memory
            .with_store(move |store| store.documents(workspace).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn prepared_none_can_lease_but_held_missing_candidate_never_reexecutes() {
    for held in [false, true] {
        let f = Fixture::new();
        let arguments = f.arguments();
        f.retain_prepared("create_document", arguments.clone(), held);
        let result = f.call("create_document", arguments);
        assert_eq!(result.is_ok(), !held);
        assert_eq!(f.owner.state.lock().unwrap().leases, usize::from(!held));
    }
}

#[test]
fn drained_unknown_without_store_proof_never_reexecutes_or_reports_saved() {
    let f = Fixture::new();
    let arguments = f.arguments();
    f.retain_prepared("create_document", arguments.clone(), true);
    f.owner.state.lock().unwrap().intent.as_mut().unwrap().stage =
        NativeResultStage::SettledUnknown;
    assert!(f.call("create_document", arguments).is_err());
    let state = f.owner.state.lock().unwrap();
    assert_eq!((state.leases, state.prepares, state.confirms), (0, 0, 0));
    assert_eq!(
        state.intent.as_ref().unwrap().stage,
        NativeResultStage::SettledUnknown
    );
    let workspace = f.record.workspace;
    assert!(
        f.memory
            .with_store(move |store| store.documents(workspace).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn held_creation_recovers_original_proof_before_pending_narrowing() {
    let f = Fixture::new();
    let arguments = f.arguments();
    let prepared = f.retain_prepared("create_document", arguments.clone(), true);
    let intent = f.owner.state.lock().unwrap().intent.clone().unwrap();
    koru_ai_server::native_store_backend(f.memory.client(), f.authority())
        .execute_native_write(&prepared, store_identity(&f.provenance, &intent))
        .unwrap();
    f.owner.pending.store(true, Ordering::SeqCst);
    assert_eq!(
        f.call("create_document", arguments).unwrap()["status"],
        "saved"
    );
    let state = f.owner.state.lock().unwrap();
    assert_eq!(
        (state.syncs, state.leases, state.prepares, state.confirms),
        (0, 0, 0, 1)
    );
}

#[test]
fn lost_registration_reply_recovers_verified_evidence_without_new_effect() {
    let f = Fixture::new();
    let arguments = f.arguments();
    f.owner.lose_confirm.store(true, Ordering::SeqCst);
    assert!(f.call("create_document", arguments.clone()).is_err());
    f.owner.pending.store(true, Ordering::SeqCst);
    assert_eq!(
        f.call("create_document", arguments).unwrap()["status"],
        "saved"
    );
    let state = f.owner.state.lock().unwrap();
    assert_eq!((state.leases, state.prepares, state.confirms), (1, 1, 2));
}

#[test]
fn verified_edit_republishes_without_reexecuting_or_requiring_effective_policy() {
    let f = Fixture::new();
    let created = f.call("create_document", f.arguments()).unwrap();
    let document = created["document_id"].clone();
    let read = f
        .call(
            "read_document",
            json!({"workspace_id":f.record.workspace,"document_id":document}),
        )
        .unwrap();
    let arguments = json!({"workspace_id":f.record.workspace,"document_id":document,
        "operation_id":Uuid::new_v4(),"base_revision":read["revision"],
        "operations":[{"op":"insert","parent_id":read["root_node_id"],"after_node_id":null,"kind":"paragraph","text":"Edited result"}]});
    *f.owner.state.lock().unwrap() = State::default();
    f.owner.lose_confirm.store(true, Ordering::SeqCst);
    assert!(f.call("edit_document", arguments.clone()).is_err());
    assert_eq!(
        f.owner.state.lock().unwrap().intent.as_ref().unwrap().stage,
        NativeResultStage::Verified
    );
    f.owner.pending.store(true, Ordering::SeqCst);
    assert_eq!(
        f.call("edit_document", arguments).unwrap()["status"],
        "saved"
    );
    let state = f.owner.state.lock().unwrap();
    assert_eq!(
        (state.syncs, state.leases, state.prepares, state.confirms),
        (1, 1, 1, 2)
    );
}

#[test]
fn unleased_prepared_waits_for_policy_and_then_uses_original_candidate() {
    let f = Fixture::new();
    let arguments = f.arguments();
    f.retain_prepared("create_document", arguments.clone(), false);
    let target = f
        .owner
        .state
        .lock()
        .unwrap()
        .intent
        .as_ref()
        .unwrap()
        .target;
    f.owner.pending.store(true, Ordering::SeqCst);
    assert!(f.call("create_document", arguments.clone()).is_err());
    assert_eq!(f.owner.state.lock().unwrap().leases, 0);
    f.owner.pending.store(false, Ordering::SeqCst);
    assert_eq!(
        f.call("create_document", arguments).unwrap()["document_id"],
        json!(target)
    );
    assert_eq!(f.owner.state.lock().unwrap().leases, 1);
}

#[test]
fn same_operation_with_changed_body_refuses_without_another_effect() {
    let f = Fixture::new();
    let mut arguments = f.arguments();
    f.call("create_document", arguments.clone()).unwrap();
    arguments["title"] = json!("Changed request");
    assert!(f.call("create_document", arguments).is_err());
    assert_eq!(f.owner.state.lock().unwrap().leases, 1);
}

#[test]
fn commit_time_placement_refusal_settles_without_saved_or_store_write() {
    let f = Fixture::new();
    let workspace = f.record.workspace;
    f.memory
        .with_store(move |store| {
            let other = store
                .create_profile("Moved", None, None, Locale::default(), Timestamp::now())
                .unwrap()
                .id;
            store.move_workspace(workspace, other).unwrap();
        })
        .unwrap();
    assert!(f.call("create_document", f.arguments()).is_err());
    let state = f.owner.state.lock().unwrap();
    assert_eq!(
        state.intent.as_ref().unwrap().stage,
        NativeResultStage::Refused
    );
    assert_eq!((state.leases, state.confirms), (1, 1));
    assert_eq!(f.policy.wakes.load(Ordering::SeqCst), 1);
    assert!(
        f.memory
            .with_store(move |store| store.documents(workspace).unwrap())
            .unwrap()
            .is_empty()
    );
}
