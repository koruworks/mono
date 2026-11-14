// SPDX-License-Identifier: MPL-2.0
//! Actual no-path Store effects with strict scripted private phase/ledger ports.
//! This is not a joined AI worker or GatewayState composition fixture.
use super::*;
use koru_ai_core::bots::{BotMemoryBinding, BotMemoryState, BotPage};
use koru_ai_server::{MemoryBaseSelection, NativeStoreAuthority};
use koru_mcp_server::native::NativeStoreIdentity;
use koru_mcp_server::native::memory::{MemoryEditRecipe, MemoryExistingChild, MemoryRecipeChange};
use koru_mcp_server::{RequestContext, WorkspaceToolExecutor};
use koru_store_core::{Locale, Timestamp, model::Scope};
use koru_store_server::{StoreWorkerClient, memory::MemoryStore};
use std::collections::BTreeMap;
use std::sync::Mutex;

struct State {
    subject: MemoryWriteSubjectEvidence,
    authority: MemoryEditAuthorityV1,
    barrier: BotPolicyBarrier,
    fence: BotExecutionFence,
    captured: CapturedBotPolicy,
    base: MemoryBaseEvidence,
    source: DocumentId,
    allocation: BTreeMap<Uuid, MemoryEditAllocation>,
    intents: BTreeMap<Uuid, NativeResultIntentView>,
    trace: Vec<&'static str>,
    leases: usize,
    wakes: usize,
    recoveries: usize,
    ensure_state: BotMemoryState,
    seed_revision: Vec<u8>,
    lost: Option<&'static str>,
    invalid_history: bool,
    pending_policy: bool,
    foreign_profile: bool,
    composite_fault: Option<(&'static str, bool)>,
    stale_final_fence: bool,
    cancel_at_base: bool,
    cancel_after_lease: bool,
    human_edit: bool,
    unleased: bool,
    bad_execute: Option<&'static str>,
    token: Option<Cancellation>,
}
struct Client {
    state: Arc<Mutex<State>>,
    store: StoreWorkerClient,
    scope: NativeStoreAuthority,
}
struct Owner(Arc<Mutex<State>>);
fn read(store: &StoreWorkerClient, scope: &NativeStoreAuthority, page: DocumentId) -> Value {
    WorkspaceToolExecutor::new(koru_ai_server::native_store_backend(
        store.clone(),
        scope.clone(),
    ))
    .invoke(
        &RequestContext::selected(
            "local",
            "memory-test",
            [Scope::ReadDocuments],
            [scope.workspace],
        ),
        "read_document",
        json!({"workspace_id":scope.workspace,"document_id":page}),
    )
    .unwrap()
}
fn revision(value: &Value) -> Vec<u8> {
    let text = value["revision"].as_str().unwrap();
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn text(node: &Value) -> &str {
    node["content"]["text"].as_str().unwrap()
}
fn node(view: &Value, id: Uuid) -> &Value {
    view["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node_id"] == id.to_string())
        .unwrap()
}
fn assert_same_allocation_identity(before: &MemoryEditAllocation, after: &MemoryEditAllocation) {
    let mut expected = before.clone();
    expected.resolution = after.resolution.clone();
    assert_eq!(&expected, after);
}
fn assert_source_pill(view: &Value, allocation: &MemoryEditAllocation) {
    let source = node(view, allocation.source_id.unwrap());
    assert_eq!(source["parent_id"], allocation.fact_id.to_string());
    let attributes = source["content"]["attributes"].as_array().unwrap();
    let pill = attributes
        .iter()
        .find(|a| a["value"]["type"] == "document")
        .unwrap();
    assert_eq!(pill["label"], "Source Run");
    assert_eq!(
        pill["value"]["document_id"],
        allocation.source_run_document.to_string()
    );
    assert!(pill["value"]["node_id"].is_null());
    assert_ne!(
        allocation.source_run_document.as_uuid(),
        allocation.key.run.as_uuid()
    );
}
impl WritePort for Client {
    fn policy(&self, r: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        let mut s = self.state.lock().unwrap();
        assert_eq!(r.provenance.owner, s.subject.subject.owner);
        let final_phase = r.target.is_some();
        s.trace.push(if final_phase {
            "composite_final"
        } else {
            "composite_initial"
        });
        let mut evidence = NativePolicyEvidence {
            profile: s.subject.profile,
            proof: FreshBotPolicyProof {
                global: if final_phase {
                    s.barrier.clone()
                } else {
                    s.subject.new_work_barrier.clone().unwrap()
                },
                routine: None,
            },
        };
        if let Some((fault, at_final)) = s.composite_fault {
            if at_final == final_phase {
                match fault {
                    "profile" => evidence.profile = koru_ai_core::ProfileId::new(),
                    "global" => {
                        evidence
                            .proof
                            .global
                            .policy
                            .selected_pages
                            .insert(DocumentId::new());
                    }
                    _ => panic!("unexpected composite fault"),
                };
            }
        }
        Ok(evidence)
    }
    fn subject(
        &self,
        r: RunMemoryWriteSubjectRequest,
    ) -> Result<MemoryWriteSubjectEvidence, ToolFailure> {
        let mut s = self.state.lock().unwrap();
        assert_eq!(r.provenance.owner, s.subject.subject.owner);
        assert_eq!(r.action, s.subject.subject.action);
        s.token = Some(r.cancellation);
        s.trace.push("subject");
        Ok(s.subject.clone())
    }
    fn base(&self, r: MemoryEditBaseRequest) -> Result<MemoryBaseEvidence, ToolFailure> {
        let mut s = self.state.lock().unwrap();
        match &r.selection {
            MemoryBaseSelection::UnallocatedHistory { version } => {
                s.trace.push("preflight");
                assert_eq!(*version, s.base.version);
            }
            MemoryBaseSelection::Allocated { allocation } => {
                s.trace.push("base");
                assert_eq!(
                    s.allocation.get(&allocation.key.operation_id),
                    Some(allocation)
                );
                if matches!(allocation.base, MemoryBaseSelector::OriginalSeed {})
                    && s.base.expected_revision != s.seed_revision
                {
                    return Err(failure("revision_conflict"));
                }
            }
        }
        if s.cancel_at_base {
            r.subject_request.cancellation.cancel();
        }
        if s.invalid_history {
            return Err(failure("memory_history_unavailable"));
        }
        Ok(s.base.clone())
    }
    fn ensure(&self, r: RunMemoryEnsureRequest) -> Result<RunMemoryEnsureResult, ToolFailure> {
        let mut s = self.state.lock().unwrap();
        s.trace.push("ensure");
        assert_eq!(
            s.allocation.get(&r.allocation.key.operation_id),
            Some(&r.allocation)
        );
        let frozen = r.allocation.ensure.as_ref().unwrap();
        assert_eq!(frozen.title, MEMORY_ENSURE_TITLE);
        assert_ne!(frozen.request_id, s.authority.creation_request); // exact alias to existing canonical intent
        Ok(RunMemoryEnsureResult {
            creation_request: s.authority.creation_request,
            state: s.ensure_state,
            observed_updated_at_ms: 10,
            binding: (s.ensure_state == BotMemoryState::Bound).then_some(BotMemoryBinding {
                owner: koru_ai_core::bots::BotOwner {
                    user_scope: s.authority.owner.user_scope,
                    device: s.authority.owner.device,
                },
                bot: s.authority.bot,
                page: BotPage {
                    workspace: s.authority.workspace,
                    document: s.authority.memory_document,
                },
                creation_request: s.authority.creation_request,
                bound_at_ms: 10,
            }),
        })
    }
    fn authority(
        &self,
        r: MemoryWriteAuthorityRequest,
    ) -> Result<MemoryWriteAuthorityEvidence, ToolFailure> {
        let mut s = self.state.lock().unwrap();
        s.trace.push("authority");
        assert_eq!(
            s.allocation.get(&r.resolved_allocation.key.operation_id),
            Some(&r.resolved_allocation)
        );
        if s.human_edit {
            let view = read(&self.store, &self.scope, s.authority.memory_document);
            let backend =
                koru_ai_server::native_store_backend(self.store.clone(), self.scope.clone());
            let prepared = backend.prepare_native_write("edit_document",json!({"workspace_id":self.scope.workspace,
                "document_id":s.authority.memory_document,"base_revision":view["revision"],"operations":[{
                    "op":"insert","parent_id":view["root_node_id"],"after_node_id":null,"kind":"paragraph","text":"Human edit"}]})).unwrap();
            backend
                .execute_native_write(
                    &prepared,
                    NativeStoreIdentity {
                        candidate: s.authority.memory_document,
                        scope: "fixture.human".into(),
                        request_key: Uuid::new_v4().to_string(),
                    },
                )
                .unwrap();
            s.human_edit = false;
        }
        Ok(MemoryWriteAuthorityEvidence {
            authority: s.authority.clone(),
            barrier: s.barrier.clone(),
            profile: if s.foreign_profile {
                koru_ai_core::ProfileId::new()
            } else {
                s.subject.profile
            },
        })
    }
    fn settled(&self) {
        self.state.lock().unwrap().wakes += 1;
    }
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), ToolFailure> {
        let mut s = self.state.lock().unwrap();
        assert_eq!(owner, s.subject.subject.owner);
        assert_eq!(bot, s.subject.subject.bot);
        assert!(s.allocation.values().any(|a| a.key == key));
        s.recoveries += 1;
        Ok(())
    }
}
impl BotDelegatePort for Owner {
    fn execute(&self, c: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut s = self.0.lock().unwrap();
        let (mut reply, step) = match c {
            BotCommand::ReadMemoryEditAllocation {
                run,
                operation_id,
                caller_fingerprint,
            } => {
                s.trace.push("lookup");
                let reply = match s.allocation.get(&operation_id) {
                    Some(a) => {
                        assert_eq!(a.key.run, run);
                        if a.caller_fingerprint != caller_fingerprint {
                            return Err(koru_delegate_api::Failure::new(
                                "bot_conflict",
                                "changed body",
                                false,
                            ));
                        }
                        BotReply::MemoryEditAllocation {
                            allocation: a.clone(),
                        }
                    }
                    None => BotReply::Missing,
                };
                (reply, "lookup")
            }
            BotCommand::SynchronizePolicy { barrier } => {
                s.trace.push("sync");
                assert_eq!(barrier, s.barrier);
                (
                    BotReply::PolicyBarrier {
                        receipt: BotPolicyBarrierReceipt {
                            barrier,
                            state: if s.pending_policy {
                                BotPolicyBarrierState::Pending
                            } else {
                                BotPolicyBarrierState::Effective
                            },
                        },
                    },
                    "sync",
                )
            }
            BotCommand::Reconcile { key } => {
                s.trace.push("reconcile");
                let mut fence = s.fence.clone();
                if s.stale_final_fence && s.trace.iter().filter(|v| **v == "sync").count() > 1 {
                    fence.generation = fence.generation.next();
                }
                (
                    BotReply::Admission {
                        admission: BotAdmissionView {
                            key,
                            run: fence.run,
                            state: BotAdmissionState::Executing,
                            queue_sequence: 1,
                            fence: Some(fence),
                        },
                    },
                    "reconcile",
                )
            }
            BotCommand::EffectFence { fence } => {
                s.trace.push("fence");
                assert_eq!(fence, s.fence);
                (
                    BotReply::EffectFence {
                        fence,
                        policy: s.captured.clone(),
                    },
                    "fence",
                )
            }
            BotCommand::AllocateMemoryEdit { request: r } => {
                s.trace.push("allocate");
                assert!(r.valid());
                assert_eq!(r.subject, s.subject.subject);
                let mut a = MemoryEditAllocation {
                    key: r.key.clone(),
                    caller_fingerprint: r.caller_fingerprint,
                    subject: r.subject,
                    base: r.base,
                    ensure: matches!(r.base, MemoryBaseSelector::OriginalSeed {}).then(|| {
                        MemoryEnsureIdentity {
                            request_id: BotRequestId::mint(),
                            expected_revision: r.current_config_revision,
                            title: MEMORY_ENSURE_TITLE.into(),
                        }
                    }),
                    resolution: MemoryBaseResolution::Unresolved {},
                    source_run_document: s.source,
                    fact_id: r.existing_fact_id.unwrap_or_else(Uuid::new_v4),
                    detail_id: None,
                    source_id: None,
                    created_at_ms: 20,
                };
                if a.subject.action != MemoryEditAction::Forget {
                    a.detail_id = Some(Uuid::new_v4());
                    a.source_id = Some(Uuid::new_v4());
                }
                assert!(a.valid());
                assert!(s.allocation.insert(r.key.operation_id, a.clone()).is_none());
                (BotReply::MemoryEditAllocation { allocation: a }, "allocate")
            }
            BotCommand::ResolveMemoryEditBase {
                key,
                caller_fingerprint,
                authority,
                expected_version,
                expected_revision,
                ..
            } => {
                s.trace.push("resolve");
                let a = s.allocation.get_mut(&key.operation_id).unwrap();
                assert_eq!(a.key, key);
                assert_eq!(a.caller_fingerprint, caller_fingerprint);
                match &a.resolution {
                    MemoryBaseResolution::Unresolved {} => {
                        a.resolution = MemoryBaseResolution::Resolved {
                            authority,
                            version: expected_version,
                            expected_revision,
                        }
                    }
                    MemoryBaseResolution::Resolved {
                        authority: old,
                        version,
                        expected_revision: bytes,
                    } => {
                        assert_eq!(old, &authority);
                        assert_eq!(*version, expected_version);
                        assert!(same_native_edit_revision(bytes, &expected_revision).unwrap());
                    }
                }
                assert!(a.valid());
                (
                    BotReply::MemoryEditAllocation {
                        allocation: a.clone(),
                    },
                    "resolve",
                )
            }
            BotCommand::PrepareMemoryEdit {
                request: r,
                caller_fingerprint,
                authority,
            } => {
                s.trace.push("prepare");
                let a = s.allocation.get(&r.key.operation_id).unwrap();
                assert_eq!(a.key, r.key);
                assert_eq!(a.caller_fingerprint, caller_fingerprint);
                assert!(
                    matches!(&a.resolution,MemoryBaseResolution::Resolved{authority:old,expected_revision,..} if *old==authority&&Some(expected_revision)==r.expected_revision.as_ref())
                );
                assert_eq!(
                    r.title,
                    koru_mcp_server::native::memory::MEMORY_REQUEST_TITLE
                );
                let intent = NativeResultIntentView {
                    purpose: NativeEffectPurpose::Memory { authority },
                    key: r.key.clone(),
                    kind: r.kind,
                    destination: r.destination,
                    request_fingerprint: r.request_fingerprint,
                    target: r.target.unwrap(),
                    store_request: Uuid::new_v4(),
                    stage: NativeResultStage::Prepared,
                    verified: None,
                    disposition: if s.unleased {
                        NativeResultDisposition::ReconcileOnly
                    } else {
                        NativeResultDisposition::Execute
                    },
                };
                assert!(
                    s.intents
                        .insert(r.key.operation_id, intent.clone())
                        .is_none()
                );
                if !s.unleased {
                    s.leases += 1;
                }
                if s.cancel_after_lease {
                    s.token.as_ref().unwrap().cancel();
                }
                (BotReply::NativeResult { intent }, "prepare")
            }
            BotCommand::ReconcileNativeResult { key } => {
                s.trace.push("intent");
                let reply = match s.intents.get(&key.operation_id) {
                    None => BotReply::Missing,
                    Some(old) => {
                        assert_eq!(old.key, key);
                        let mut intent = old.clone();
                        intent.disposition = NativeResultDisposition::ReconcileOnly;
                        BotReply::NativeResult { intent }
                    }
                };
                (reply, "intent")
            }
            BotCommand::ConfirmNativeResult { request: r } => {
                s.trace.push("confirm");
                let intent = s.intents.get_mut(&r.key.operation_id).unwrap();
                assert_eq!(intent.key, r.key);
                assert_eq!(intent.request_fingerprint, r.request_fingerprint);
                match r.observation {
                    NativeResultObservation::Durable { evidence } => {
                        assert!(evidence.valid());
                        assert_eq!(evidence.document, intent.target);
                        intent.verified = Some(evidence);
                        intent.stage = NativeResultStage::Registered;
                        intent.disposition = NativeResultDisposition::AlreadyRegistered;
                    }
                    NativeResultObservation::NotApplied { .. } => {
                        intent.stage = NativeResultStage::Refused
                    }
                    NativeResultObservation::Unconfirmed => {
                        intent.stage = NativeResultStage::Unconfirmed
                    }
                    _ => panic!("unexpected observation"),
                }
                (
                    BotReply::NativeResult {
                        intent: intent.clone(),
                    },
                    "confirm",
                )
            }
            _ => panic!("unexpected command in Memory execution"),
        };
        if step == "prepare" {
            if let BotReply::NativeResult { intent } = &mut reply {
                match s.bad_execute {
                    Some("stage") => intent.stage = NativeResultStage::Verified,
                    Some("evidence") => {
                        intent.verified = Some(NativeResultReference {
                            operation_id: intent.key.operation_id,
                            workspace: intent.destination,
                            document: intent.target,
                            version: s.base.expected_revision.clone(),
                            title: "Memory".into(),
                            confirmed_at_ms: 20,
                            kind: NativeResultKind::EditedPage,
                        })
                    }
                    Some("identity") => intent.key.operation_id = Uuid::new_v4(),
                    None => {}
                    _ => panic!("unknown malformed Execute fixture"),
                }
            }
        }
        if s.lost == Some(step) {
            s.lost = None;
            return Err(koru_delegate_api::Failure::new(
                "delegate_unavailable",
                "lost reply",
                true,
            ));
        }
        Ok(reply)
    }
}

struct Fixture {
    memory: MemoryStore,
    client: Client,
    owner: Owner,
    record: RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    state: Arc<Mutex<State>>,
    args: Value,
    tool: &'static str,
}
impl Fixture {
    fn new() -> Self {
        let memory = MemoryStore::open("memory-gateway-execution").unwrap();
        let (scope, profile, workspace, source) = memory
            .with_store(|store| {
                let profile = store
                    .create_profile("Test", None, None, Locale::default(), Timestamp::now())
                    .unwrap()
                    .id;
                let workspace = store
                    .create_workspace(profile, "Work", Timestamp::now())
                    .unwrap()
                    .id;
                let source = store
                    .create_document(
                        workspace,
                        "Source Run",
                        "page",
                        None,
                        None,
                        Timestamp::now(),
                    )
                    .unwrap()
                    .0;
                (
                    store.local_user_scope().unwrap(),
                    profile,
                    workspace,
                    source,
                )
            })
            .unwrap();
        let store_authority = NativeStoreAuthority {
            owner_scope: scope,
            expected_profile: profile,
            workspace,
        };
        let backend =
            koru_ai_server::native_store_backend(memory.client(), store_authority.clone());
        let prepared = backend
            .prepare_native_write(
                "create_document",
                json!({"workspace_id":workspace,"title":"Memory"}),
            )
            .unwrap();
        let page = DocumentId::new();
        backend
            .execute_native_write(
                &prepared,
                NativeStoreIdentity {
                    candidate: page,
                    scope: "fixture.memory.seed".into(),
                    request_key: Uuid::new_v4().to_string(),
                },
            )
            .unwrap();
        let view = read(&memory.client(), &store_authority, page);
        let owner = BotOwner {
            user_scope: scope,
            device: DeviceId::new(),
        };
        let provenance = BotProvenance {
            owner,
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
            routine: None,
        };
        let captured = CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
        };
        let subject = MemoryEditSubjectV1 {
            version: 1,
            owner,
            bot: provenance.bot,
            submission: provenance.submission,
            captured_config_revision: 1,
            action: MemoryEditAction::Remember,
        };
        let authority = MemoryEditAuthorityV1 {
            version: 1,
            owner,
            bot: provenance.bot,
            submission: provenance.submission,
            captured_config_revision: 1,
            creation_request: BotRequestId::mint(),
            workspace,
            memory_document: page,
            action: MemoryEditAction::Remember,
        };
        let barrier = BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner,
                bot: provenance.bot,
                request: Uuid::new_v4().try_into().unwrap(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: captured.clone(),
            effects_allowed: true,
        };
        let fence = BotExecutionFence {
            owner,
            bot: provenance.bot,
            run: RunId::mint(),
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            session: SessionId::mint(),
            turn: Some(AgentTurnId::mint()),
        };
        let version = DocumentVersionId::new();
        let vv = revision(&view);
        let base = MemoryBaseEvidence {
            authority: authority.clone(),
            version,
            expected_revision: vv.clone(),
            recipe: MemoryEditRecipe {
                workspace,
                document: page,
                history: version.as_uuid(),
                root: Uuid::parse_str(view["root_node_id"].as_str().unwrap()).unwrap(),
                change: MemoryRecipeChange::Remember {
                    after: None,
                    summary: "Remembered".into(),
                    detail: "Detail".into(),
                },
            },
        };
        let record = RunAuthorityRecord {
            run: fence.run,
            workspace,
            generation: fence.generation,
            grants: [workspace].into(),
            run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
                if cfg!(windows) {
                    "D:/never-opened-write"
                } else {
                    "/never-opened-write"
                },
            ))
            .unwrap(),
            state: koru_artifact_core::AuthorityState::Active,
            created_at_ms: 0,
            revoked_at_ms: None,
        };
        let state = Arc::new(Mutex::new(State {
            subject: MemoryWriteSubjectEvidence {
                subject,
                profile,
                config_revision: 1,
                policy_revision: 1,
                new_work_barrier: Some(barrier.clone()),
            },
            authority,
            barrier,
            fence,
            captured: captured.clone(),
            base,
            source,
            allocation: BTreeMap::new(),
            intents: BTreeMap::new(),
            trace: vec![],
            leases: 0,
            wakes: 0,
            recoveries: 0,
            ensure_state: BotMemoryState::Bound,
            seed_revision: vv,
            lost: None,
            invalid_history: false,
            pending_policy: false,
            foreign_profile: false,
            composite_fault: None,
            stale_final_fence: false,
            cancel_at_base: false,
            cancel_after_lease: false,
            human_edit: false,
            unleased: false,
            bad_execute: None,
            token: None,
        }));
        Self {
            client: Client {
                state: state.clone(),
                store: memory.client(),
                scope: store_authority,
            },
            owner: Owner(state.clone()),
            memory,
            record,
            provenance,
            captured,
            state,
            args: json!({"operation_id":Uuid::new_v4(),"expected_version":version,"summary":"Remembered","detail":"Detail"}),
            tool: "remember_memory",
        }
    }
    fn call(&self) -> Result<Value, ToolFailure> {
        invoke(
            &self.client,
            &self.owner,
            &self.record,
            self.provenance.clone(),
            self.captured.clone(),
            self.tool,
            self.args.clone(),
            Cancellation::default(),
            Some((&self.memory.client(), true)),
        )
    }
    fn view(&self) -> Value {
        let page = self.state.lock().unwrap().authority.memory_document;
        read(&self.memory.client(), &self.client.scope, page)
    }
    fn operation(&self) -> Uuid {
        Uuid::parse_str(self.args["operation_id"].as_str().unwrap()).unwrap()
    }
    fn begin(&mut self, tool: &'static str, fact: Uuid) {
        let view = self.view();
        let version = DocumentVersionId::new();
        let mut s = self.state.lock().unwrap();
        let action = if tool == "correct_memory" {
            MemoryEditAction::Correct
        } else {
            MemoryEditAction::Forget
        };
        let current = node(&view, fact);
        s.authority.action = action;
        s.subject.subject.action = action;
        let change = if action == MemoryEditAction::Correct {
            MemoryRecipeChange::Correct {
                fact,
                expected: text(current).into(),
                children: view["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|n| n["parent_id"] == fact.to_string())
                    .map(|n| MemoryExistingChild {
                        node: Uuid::parse_str(n["node_id"].as_str().unwrap()).unwrap(),
                        expected: text(n).into(),
                    })
                    .collect(),
                summary: "Corrected".into(),
                detail: "Replacement".into(),
            }
        } else {
            MemoryRecipeChange::Forget {
                fact,
                expected: text(current).into(),
            }
        };
        s.base = MemoryBaseEvidence {
            authority: s.authority.clone(),
            version,
            expected_revision: revision(&view),
            recipe: MemoryEditRecipe {
                workspace: self.record.workspace,
                document: s.authority.memory_document,
                history: version.as_uuid(),
                root: Uuid::parse_str(view["root_node_id"].as_str().unwrap()).unwrap(),
                change,
            },
        };
        self.args =
            json!({"operation_id":Uuid::new_v4(),"expected_version":version,"fact_id":fact});
        if action == MemoryEditAction::Correct {
            self.args["summary"] = json!("Corrected");
            self.args["detail"] = json!("Replacement");
        }
        self.tool = tool;
    }
}

#[test]
fn memory_gateway_executes_all_actions_with_exact_source_pill_and_visible_removal() {
    let mut f = Fixture::new();
    let saved = f.call().unwrap();
    let fact = Uuid::parse_str(saved["fact_id"].as_str().unwrap()).unwrap();
    let first = f.state.lock().unwrap().allocation[&f.operation()].clone();
    let view = f.view();
    assert_eq!(node(&view, fact)["metadata"]["heading"], 2);
    assert_eq!(text(node(&view, first.detail_id.unwrap())), "Detail");
    assert_source_pill(&view, &first);
    f.begin("correct_memory", fact);
    assert_eq!(f.call().unwrap()["fact_id"], json!(fact));
    let view = f.view();
    assert_eq!(text(node(&view, fact)), "Corrected");
    let corrected = f.state.lock().unwrap().allocation[&f.operation()].clone();
    assert_source_pill(&view, &corrected);
    assert_eq!(
        text(node(&view, corrected.detail_id.unwrap())),
        "Replacement"
    );
    // The ordinary MCP projection excludes Trash. W1a's real core test owns
    // retained text/pill/restore proof; this composition checks visible removal.
    for id in [first.detail_id.unwrap(), first.source_id.unwrap()] {
        assert!(
            !view["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["node_id"] == id.to_string())
        );
    }
    f.begin("forget_memory", fact);
    assert_eq!(f.call().unwrap()["fact_id"], json!(fact));
    let view = f.view();
    for id in [
        fact,
        corrected.detail_id.unwrap(),
        corrected.source_id.unwrap(),
    ] {
        assert!(
            !view["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["node_id"] == id.to_string())
        );
    }
    let s = f.state.lock().unwrap();
    assert_eq!(s.leases, 3);
    assert_eq!(s.wakes, 3);
    assert!(
        s.intents
            .values()
            .all(|v| matches!(v.purpose, NativeEffectPurpose::Memory { .. })
                && v.stage == NativeResultStage::Registered)
    );
}
#[test]
fn memory_gateway_lazy_pending_alias_reuses_ids_and_existing_page() {
    let mut f = Fixture::new();
    f.args.as_object_mut().unwrap().remove("expected_version");
    f.state.lock().unwrap().ensure_state = BotMemoryState::Prepared;
    let before = f.view();
    let error = f.call().unwrap_err();
    assert_eq!(error.code, "memory_creation_pending");
    assert_eq!(error.details.unwrap()["operation_id"], json!(f.operation()));
    let initial = f.state.lock().unwrap().allocation[&f.operation()].clone();
    assert_eq!(f.view(), before);
    f.state.lock().unwrap().ensure_state = BotMemoryState::Bound;
    let saved = f.call().unwrap();
    assert_eq!(saved["fact_id"], json!(initial.fact_id));
    let s = f.state.lock().unwrap();
    assert_eq!(s.allocation.len(), 1);
    assert_eq!(s.leases, 1);
    assert_ne!(
        initial.ensure.as_ref().unwrap().request_id,
        s.authority.creation_request
    );
    assert_same_allocation_identity(&initial, &s.allocation[&f.operation()]);
}
#[test]
fn memory_gateway_creation_conflict_or_abandoned_never_autoretries() {
    for state in [BotMemoryState::Conflict, BotMemoryState::Abandoned] {
        let mut f = Fixture::new();
        f.args.as_object_mut().unwrap().remove("expected_version");
        f.state.lock().unwrap().ensure_state = state;
        let code = if state == BotMemoryState::Conflict {
            "memory_creation_conflict"
        } else {
            "memory_creation_abandoned"
        };
        assert_eq!(f.call().unwrap_err().code, code);
        assert_eq!(f.call().unwrap_err().code, code);
        let s = f.state.lock().unwrap();
        assert_eq!(s.allocation.len(), 1);
        assert!(s.intents.is_empty());
        assert!(!s.trace.contains(&"base"));
    }
}
#[test]
fn memory_gateway_invalid_history_policy_or_cancellation_never_allocates() {
    for fault in 0..3 {
        let f = Fixture::new();
        {
            let mut s = f.state.lock().unwrap();
            match fault {
                0 => s.invalid_history = true,
                1 => s.pending_policy = true,
                _ => s.cancel_at_base = true,
            }
        }
        assert!(f.call().is_err());
        let s = f.state.lock().unwrap();
        assert!(!s.trace.contains(&"allocate"));
        assert!(s.intents.is_empty());
    }
}
#[test]
fn memory_gateway_profile_or_stale_final_fence_refuses_before_lease() {
    for foreign in [true, false] {
        let f = Fixture::new();
        let before = f.view();
        {
            let mut s = f.state.lock().unwrap();
            s.foreign_profile = foreign;
            s.stale_final_fence = !foreign;
        }
        assert!(f.call().is_err());
        assert_eq!(f.view(), before);
        let s = f.state.lock().unwrap();
        assert_eq!(s.leases, 0);
        assert!(!s.trace.contains(&"prepare"));
    }
}

#[test]
fn memory_gateway_composite_matches_each_phase_profile_and_full_global_payload() {
    for final_phase in [false, true] {
        for fault in ["profile", "global"] {
            let f = Fixture::new();
            f.state.lock().unwrap().composite_fault = Some((fault, final_phase));
            assert_eq!(f.call().unwrap_err().code, "permission_denied");
            let s = f.state.lock().unwrap();
            assert_eq!(s.leases, 0);
            assert!(!s.trace.contains(&"prepare"));
            assert_eq!(s.trace.contains(&"allocate"), final_phase);
            assert!(s.trace.contains(&"composite_initial"));
            assert_eq!(s.trace.contains(&"composite_final"), final_phase);
        }
    }
}

#[test]
fn memory_gateway_retained_receipt_precedes_composite_freshness_but_new_work_refuses() {
    let mut f = Fixture::new();
    let saved = f.call().unwrap();
    {
        let mut s = f.state.lock().unwrap();
        s.trace.clear();
        s.subject.new_work_barrier = None;
        s.barrier.effects_allowed = false;
        s.pending_policy = true;
        s.composite_fault = Some(("profile", false));
    }
    assert_eq!(f.call().unwrap(), saved);
    {
        let s = f.state.lock().unwrap();
        assert_eq!(s.leases, 1);
        assert!(!s.trace.contains(&"composite_initial"));
        assert!(!s.trace.contains(&"composite_final"));
        assert!(!s.trace.contains(&"sync"));
    }
    f.args["summary"] = json!("Changed caller request");
    assert_eq!(f.call().unwrap_err().code, "idempotency_conflict");
    f.args["operation_id"] = json!(Uuid::new_v4());
    assert!(f.call().is_err());
    assert_eq!(f.state.lock().unwrap().leases, 1);
}
#[test]
fn memory_gateway_human_revision_conflict_settles_without_memory_effect() {
    let f = Fixture::new();
    f.state.lock().unwrap().human_edit = true;
    assert_eq!(f.call().unwrap_err().code, "revision_conflict");
    let s = f.state.lock().unwrap();
    assert_eq!(s.leases, 1);
    assert_eq!(s.wakes, 1);
    assert_eq!(s.intents[&f.operation()].stage, NativeResultStage::Refused);
    let fact = s.allocation[&f.operation()].fact_id;
    drop(s);
    let view = f.view();
    assert!(
        view["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["content"]["text"] == "Human edit")
    );
    assert!(
        !view["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["node_id"] == fact.to_string())
    );
}
#[test]
fn memory_gateway_lost_allocate_resolve_confirm_retries_one_effect() {
    for step in ["allocate", "resolve", "confirm"] {
        let f = Fixture::new();
        f.state.lock().unwrap().lost = Some(step);
        assert!(f.call().is_err());
        let initial = f.state.lock().unwrap().allocation[&f.operation()].clone();
        assert_eq!(f.call().unwrap()["status"], "saved");
        let s = f.state.lock().unwrap();
        assert_eq!(s.allocation.len(), 1);
        assert_eq!(s.leases, 1);
        assert_same_allocation_identity(&initial, &s.allocation[&f.operation()]);
    }
}
#[test]
fn memory_gateway_lost_prepare_and_unleased_rows_never_execute_again() {
    for lost in [true, false] {
        let f = Fixture::new();
        let before = f.view();
        {
            let mut s = f.state.lock().unwrap();
            if lost {
                s.lost = Some("prepare");
            } else {
                s.unleased = true;
            }
        }
        assert_eq!(f.call().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.call().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.view(), before);
        let s = f.state.lock().unwrap();
        assert_eq!(s.leases, usize::from(lost));
        assert!(s.recoveries >= 2);
        assert_eq!(s.trace.iter().filter(|v| **v == "prepare").count(), 1);
    }
}
#[test]
fn memory_gateway_cancellation_after_execute_still_settles_the_lease() {
    let f = Fixture::new();
    f.state.lock().unwrap().cancel_after_lease = true;
    assert_eq!(f.call().unwrap()["status"], "saved");
    let s = f.state.lock().unwrap();
    assert!(s.token.as_ref().unwrap().is_cancelled());
    assert_eq!(s.leases, 1);
    assert_eq!(s.wakes, 1);
    assert_eq!(
        s.intents[&f.operation()].stage,
        NativeResultStage::Registered
    );
}

#[test]
fn memory_gateway_malformed_execute_echo_recovers_known_key_without_effect() {
    for fault in ["stage", "evidence", "identity"] {
        let f = Fixture::new();
        let before = f.view();
        f.state.lock().unwrap().bad_execute = Some(fault);
        assert_eq!(f.call().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.call().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.view(), before);
        let s = f.state.lock().unwrap();
        assert_eq!(s.leases, 1);
        assert_eq!(s.recoveries, 2);
        assert_eq!(s.wakes, 0);
        assert_eq!(s.trace.iter().filter(|v| **v == "prepare").count(), 1);
        assert!(!s.trace.contains(&"confirm"));
    }
}

#[test]
fn memory_gateway_lost_not_applied_recovers_and_retry_never_executes() {
    let f = Fixture::new();
    {
        let mut s = f.state.lock().unwrap();
        s.human_edit = true;
        s.lost = Some("confirm");
    }
    assert_eq!(f.call().unwrap_err().code, "save_status_unknown");
    let after_human_edit = f.view();
    assert_eq!(f.call().unwrap_err().code, "native_request_refused");
    assert_eq!(f.view(), after_human_edit);
    let s = f.state.lock().unwrap();
    assert_eq!(s.leases, 1);
    assert_eq!(s.recoveries, 1);
    assert_eq!(s.wakes, 0);
    assert_eq!(s.trace.iter().filter(|v| **v == "prepare").count(), 1);
    assert_eq!(s.trace.iter().filter(|v| **v == "confirm").count(), 1);
    assert_eq!(s.intents[&f.operation()].stage, NativeResultStage::Refused);
}
