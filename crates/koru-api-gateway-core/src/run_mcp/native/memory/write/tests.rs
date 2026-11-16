// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::cell::{Cell, RefCell};
use std::sync::Mutex;

struct Client {
    subject: MemoryWriteSubjectEvidence,
    reads: Cell<usize>,
    wakes: Cell<usize>,
    recovery: RefCell<Vec<NativeResultKey>>,
}
impl WritePort for Client {
    fn policy(&self, _: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        panic!("retained receipt must not request fresh composite proof")
    }
    fn subject(
        &self,
        request: RunMemoryWriteSubjectRequest,
    ) -> Result<MemoryWriteSubjectEvidence, ToolFailure> {
        assert_eq!(request.action, self.subject.subject.action);
        self.reads.set(self.reads.get() + 1);
        Ok(self.subject.clone())
    }
    fn base(&self, _: MemoryEditBaseRequest) -> Result<MemoryBaseEvidence, ToolFailure> {
        panic!("receipt must not read old history")
    }
    fn ensure(&self, _: RunMemoryEnsureRequest) -> Result<RunMemoryEnsureResult, ToolFailure> {
        panic!("receipt must not create Memory")
    }
    fn authority(
        &self,
        _: MemoryWriteAuthorityRequest,
    ) -> Result<MemoryWriteAuthorityEvidence, ToolFailure> {
        panic!("receipt must not request fresh permission")
    }
    fn settled(&self) {
        self.wakes.set(self.wakes.get() + 1);
    }
    fn uncertain(
        &self,
        owner: BotOwner,
        bot: BotId,
        key: NativeResultKey,
    ) -> Result<(), ToolFailure> {
        assert_eq!(owner, self.subject.subject.owner);
        assert_eq!(bot, self.subject.subject.bot);
        self.recovery.borrow_mut().push(key);
        Ok(())
    }
}
struct State {
    allocation: Option<MemoryEditAllocation>,
    intent: Option<NativeResultIntentView>,
    commands: Vec<&'static str>,
    lose_confirm: bool,
    wrong_purpose: bool,
    wrong_store_request: bool,
}
struct Owner(Mutex<State>);
impl BotDelegatePort for Owner {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut s = self.0.lock().unwrap();
        Ok(match command {
            BotCommand::ReadMemoryEditAllocation {
                run,
                operation_id,
                caller_fingerprint,
            } => {
                s.commands.push("allocation");
                match &s.allocation {
                    None => BotReply::Missing,
                    Some(a) => {
                        // The owner returns only the caller's exact retained operation.
                        if a.key.run != run
                            || a.key.operation_id != operation_id
                            || a.caller_fingerprint != caller_fingerprint
                        {
                            return Err(koru_delegate_api::Failure::new(
                                "bot_conflict",
                                "Retained caller fingerprint differs",
                                false,
                            ));
                        }
                        BotReply::MemoryEditAllocation {
                            allocation: a.clone(),
                        }
                    }
                }
            }
            BotCommand::ReconcileNativeResult { key } => {
                s.commands.push("reconcile");
                match &s.intent {
                    None => BotReply::Missing,
                    Some(intent) => {
                        assert_eq!(intent.key, key);
                        BotReply::NativeResult {
                            intent: intent.clone(),
                        }
                    }
                }
            }
            BotCommand::ConfirmNativeResult { request } => {
                s.commands.push("confirm");
                let NativeResultObservation::Durable { evidence } = request.observation else {
                    panic!("receipt only confirms retained evidence")
                };
                let intent = s.intent.as_mut().unwrap();
                assert_eq!(request.key, intent.key);
                assert_eq!(request.request_fingerprint, intent.request_fingerprint);
                assert_eq!(intent.verified.as_ref(), Some(&evidence));
                intent.stage = NativeResultStage::Registered;
                intent.disposition = NativeResultDisposition::AlreadyRegistered;
                let mut reply = intent.clone();
                if std::mem::take(&mut s.lose_confirm) {
                    return Err(koru_delegate_api::Failure::new(
                        "delegate_unavailable",
                        "Lost confirmation response",
                        true,
                    ));
                }
                if s.wrong_purpose {
                    reply.purpose = NativeEffectPurpose::Result {};
                }
                if s.wrong_store_request {
                    reply.store_request = Uuid::new_v4();
                }
                BotReply::NativeResult { intent: reply }
            }
            _ => {
                panic!("receipt must not synchronize policy, allocate, prepare or acquire a lease")
            }
        })
    }
}
struct Fixture {
    client: Client,
    owner: Owner,
    record: RunAuthorityRecord,
    provenance: BotProvenance,
    captured: CapturedBotPolicy,
    args: Value,
}
impl Fixture {
    fn new(stage: NativeResultStage) -> Self {
        let workspace = WorkspaceId::new();
        let owner = BotOwner {
            user_scope: koru_store_core::UserScopeId::new(),
            device: DeviceId::new(),
        };
        let provenance = BotProvenance {
            parent: None,
            routine: None,
            owner,
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
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
            memory_document: DocumentId::new(),
            action: MemoryEditAction::Remember,
        };
        let key = NativeResultKey {
            run: RunId::mint(),
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            turn: AgentTurnId::mint(),
            operation_id: Uuid::new_v4(),
        };
        let version = DocumentVersionId::new();
        let base = MemoryBaseSelector::History { version };
        let args = json!({"operation_id":key.operation_id,"expected_version":version,"summary":"Exact summary","detail":"Exact detail"});
        let operation = args::parse("remember_memory", args.clone()).unwrap();
        let allocation = MemoryEditAllocation {
            key: key.clone(),
            caller_fingerprint: memory_write_fingerprint(
                &subject,
                key.run,
                key.operation_id,
                base,
                &operation.operation,
            )
            .unwrap(),
            subject: subject.clone(),
            base,
            ensure: None,
            resolution: MemoryBaseResolution::Resolved {
                authority: authority.clone(),
                version,
                expected_revision: vec![2, 1, 2, 2, 4],
            },
            source_run_document: DocumentId::new(),
            fact_id: Uuid::new_v4(),
            detail_id: Some(Uuid::new_v4()),
            source_id: Some(Uuid::new_v4()),
            created_at_ms: 1,
        };
        assert!(allocation.valid());
        let intent = NativeResultIntentView {
            purpose: NativeEffectPurpose::Memory { authority },
            key: key.clone(),
            kind: NativeResultOperation::EditPage,
            destination: workspace,
            request_fingerprint: [7; 32],
            target: match &allocation.resolution {
                MemoryBaseResolution::Resolved { authority, .. } => authority.memory_document,
                _ => unreachable!(),
            },
            store_request: Uuid::new_v4(),
            stage,
            disposition: NativeResultDisposition::ReconcileOnly,
            verified: if matches!(
                stage,
                NativeResultStage::Registered | NativeResultStage::Verified
            ) {
                Some(NativeResultReference {
                    version_id: None,
                    operation_id: key.operation_id,
                    workspace,
                    document: match &allocation.resolution {
                        MemoryBaseResolution::Resolved { authority, .. } => {
                            authority.memory_document
                        }
                        _ => unreachable!(),
                    },
                    version: vec![2, 1, 2, 2, 4],
                    title: "Memory".into(),
                    confirmed_at_ms: 5,
                    kind: NativeResultKind::EditedPage,
                })
            } else {
                None
            },
        };
        Self {
            client: Client {
                subject: MemoryWriteSubjectEvidence {
                    subject,
                    profile: koru_ai_core::ProfileId::new(),
                    config_revision: 8,
                    policy_revision: 5,
                    new_work_barrier: None,
                },
                reads: Cell::new(0),
                wakes: Cell::new(0),
                recovery: RefCell::new(vec![]),
            },
            owner: Owner(Mutex::new(State {
                allocation: Some(allocation),
                intent: Some(intent),
                commands: vec![],
                lose_confirm: false,
                wrong_purpose: false,
                wrong_store_request: false,
            })),
            record: RunAuthorityRecord {
                run: key.run,
                workspace,
                generation: key.generation,
                grants: [workspace].into(),
                run_workspace: koru_artifact_core::RunWorkspace::new(std::path::Path::new(
                    if cfg!(windows) {
                        "D:/never-opened-memory-receipt"
                    } else {
                        "/never-opened-memory-receipt"
                    },
                ))
                .unwrap(),
                state: koru_artifact_core::AuthorityState::Active,
                created_at_ms: 0,
                revoked_at_ms: None,
            },
            provenance,
            captured: CapturedBotPolicy {
                workspace,
                destination: workspace,
                access: BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            args,
        }
    }
    fn invoke(&self) -> Result<Value, ToolFailure> {
        invoke(
            &self.client,
            &self.owner,
            &self.record,
            self.provenance.clone(),
            self.captured.clone(),
            "remember_memory",
            self.args.clone(),
            Cancellation::default(),
            None,
        )
    }
}

#[test]
fn write_arguments_are_closed_bounded_and_canonical_before_any_port() {
    let mut f = Fixture::new(NativeResultStage::Registered);
    for (field, value) in [
        ("owner", json!("foreign")),
        ("expected_version", Value::Null),
        (
            "operation_id",
            json!("AAAAAAAA-0000-4000-8000-000000000001"),
        ),
        ("operation_id", json!(Uuid::nil())),
        ("summary", json!("a\nb")),
        ("detail", json!("x".repeat(16385))),
        ("detail", json!("\"".repeat(16384))),
    ] {
        let original = f.args.clone();
        f.args[field] = value;
        assert!(f.invoke().is_err(), "{field}");
        assert_eq!(f.client.reads.get(), 0);
        assert!(f.owner.0.lock().unwrap().commands.is_empty());
        f.args = original;
    }
}
#[test]
fn write_arguments_keep_exact_text_and_full_fact_uuid_domain() {
    for fact in [
        Uuid::nil(),
        Uuid::parse_str("00000000-0000-5000-8000-000000000001").unwrap(),
    ] {
        let parsed = args::parse("correct_memory", json!({"operation_id":Uuid::new_v4(),"expected_version":DocumentVersionId::new(),"fact_id":fact,"summary":"  exact  ","detail":"quoted \" target \\ data"})).unwrap();
        let MemoryRecipeOperation::Correct {
            fact: actual,
            summary,
            detail,
        } = parsed.operation
        else {
            panic!("wrong action")
        };
        assert_eq!(actual, fact);
        assert_eq!(summary, "  exact  ");
        assert_eq!(detail, "quoted \" target \\ data");
        assert!(args::parse("forget_memory", json!({"operation_id":Uuid::new_v4(),"expected_version":DocumentVersionId::new(),"fact_id":fact})).is_ok());
    }
    let parsed = args::parse(
        "remember_memory",
        json!({"operation_id":Uuid::new_v4(),"summary":"Seed","detail":"Original only"}),
    )
    .unwrap();
    assert_eq!(parsed.base, MemoryBaseSelector::OriginalSeed {});
    assert!(args::parse("correct_memory", json!({"operation_id":Uuid::new_v4(),"fact_id":Uuid::new_v4(),"summary":"S","detail":"D"})).is_err());
}
#[test]
fn registered_memory_receipt_needs_no_history_fresh_authority_or_result_publication() {
    let f = Fixture::new(NativeResultStage::Registered);
    let saved = f.invoke().unwrap();
    let s = f.owner.0.lock().unwrap();
    let a = s.allocation.as_ref().unwrap();
    assert_eq!(
        saved,
        json!({"status":"saved","operation_id":a.key.operation_id,"action":"remember","fact_id":a.fact_id})
    );
    assert_eq!(s.commands, ["allocation", "reconcile"]);
    assert_eq!(f.client.wakes.get(), 0);
    assert!(f.client.recovery.borrow().is_empty());
    assert!(serde_json::to_vec(&saved).unwrap().len() <= 4096);
}

#[test]
fn correct_and_forget_retained_receipts_preserve_full_domain_fact_identity() {
    for fact in [
        Uuid::nil(),
        Uuid::parse_str("00000000-0000-5000-8000-000000000001").unwrap(),
    ] {
        for tool in ["correct_memory", "forget_memory"] {
            let mut f = Fixture::new(NativeResultStage::Registered);
            f.args["fact_id"] = json!(fact);
            if tool == "forget_memory" {
                f.args.as_object_mut().unwrap().remove("summary");
                f.args.as_object_mut().unwrap().remove("detail");
            }
            let parsed = args::parse(tool, f.args.clone()).unwrap();
            f.client.subject.subject.action = parsed.action;
            {
                let mut state = f.owner.0.lock().unwrap();
                let a = state.allocation.as_mut().unwrap();
                a.subject.action = parsed.action;
                a.fact_id = fact;
                if tool == "forget_memory" {
                    a.detail_id = None;
                    a.source_id = None;
                }
                a.caller_fingerprint = memory_write_fingerprint(
                    &a.subject,
                    a.key.run,
                    parsed.operation_id,
                    parsed.base,
                    &parsed.operation,
                )
                .unwrap();
                let MemoryBaseResolution::Resolved { authority, .. } = &mut a.resolution else {
                    unreachable!()
                };
                authority.action = parsed.action;
                let authority = authority.clone();
                assert!(a.valid());
                state.intent.as_mut().unwrap().purpose = NativeEffectPurpose::Memory { authority };
            }
            let saved = invoke(
                &f.client,
                &f.owner,
                &f.record,
                f.provenance.clone(),
                f.captured.clone(),
                tool,
                f.args.clone(),
                Cancellation::default(),
                None,
            )
            .unwrap();
            assert_eq!(saved["status"], "saved");
            assert_eq!(saved["fact_id"], json!(fact));
            assert_eq!(saved["action"], json!(parsed.action));
            assert_eq!(
                f.owner.0.lock().unwrap().commands,
                ["allocation", "reconcile"]
            );
            assert_eq!(f.client.wakes.get(), 0);
        }
    }
}

#[test]
fn changed_memory_body_preserves_known_conflict_before_reconcile() {
    let mut f = Fixture::new(NativeResultStage::Registered);
    f.args["detail"] = json!("Changed retry");
    let error = f.invoke().unwrap_err();
    assert_eq!(error.code, "idempotency_conflict");
    assert!(error.message.contains("original operation"));
    assert_eq!(f.owner.0.lock().unwrap().commands, ["allocation"]);
    assert!(f.client.recovery.borrow().is_empty());
}
#[test]
fn verified_memory_receipt_confirms_exact_purpose_then_wakes_without_a_new_effect() {
    let f = Fixture::new(NativeResultStage::Verified);
    assert_eq!(f.invoke().unwrap()["status"], "saved");
    assert_eq!(f.client.wakes.get(), 1);
    assert_eq!(
        f.owner.0.lock().unwrap().commands,
        ["allocation", "reconcile", "confirm"]
    );
    assert_eq!(f.invoke().unwrap()["status"], "saved");
    assert_eq!(f.client.wakes.get(), 1);
}
#[test]
fn memory_confirmation_rejects_purpose_and_store_identity_mismatch() {
    for wrong_purpose in [true, false] {
        let f = Fixture::new(NativeResultStage::Verified);
        {
            let mut s = f.owner.0.lock().unwrap();
            s.wrong_purpose = wrong_purpose;
            s.wrong_store_request = !wrong_purpose;
        }
        assert_eq!(f.invoke().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.client.wakes.get(), 0);
    }
}
#[test]
fn memory_confirmation_loss_retries_retained_receipt_without_reexecution() {
    let f = Fixture::new(NativeResultStage::Verified);
    f.owner.0.lock().unwrap().lose_confirm = true;
    assert_eq!(f.invoke().unwrap_err().code, "save_status_unknown");
    assert_eq!(f.invoke().unwrap()["status"], "saved");
    assert_eq!(
        f.owner.0.lock().unwrap().commands,
        [
            "allocation",
            "reconcile",
            "confirm",
            "allocation",
            "reconcile"
        ]
    );
}
#[test]
fn retained_memory_identity_and_evidence_mismatch_never_confirms() {
    for mutation in 0..7 {
        let f = Fixture::new(NativeResultStage::Verified);
        {
            let mut s = f.owner.0.lock().unwrap();
            match mutation {
                0 => s.intent.as_mut().unwrap().purpose = NativeEffectPurpose::Result {},
                1 => s.intent.as_mut().unwrap().target = DocumentId::new(),
                2 => s.intent.as_mut().unwrap().destination = WorkspaceId::new(),
                3 => s.intent.as_mut().unwrap().store_request = Uuid::nil(),
                4 => {
                    s.intent.as_mut().unwrap().verified.as_mut().unwrap().kind =
                        NativeResultKind::CreatedPage
                }
                5 => s.allocation.as_mut().unwrap().subject.bot = BotId::mint(),
                6 => {
                    s.allocation.as_mut().unwrap().key.generation = AttemptGeneration::FIRST.next()
                }
                _ => unreachable!(),
            }
        }
        assert!(f.invoke().is_err(), "mutation {mutation}");
        assert!(!f.owner.0.lock().unwrap().commands.contains(&"confirm"));
        assert_eq!(f.client.wakes.get(), 0);
    }
}
#[test]
fn unresolved_memory_receipts_only_request_recovery_and_refused_stays_refused() {
    for stage in [
        NativeResultStage::Prepared,
        NativeResultStage::Unconfirmed,
        NativeResultStage::SettledUnknown,
    ] {
        let f = Fixture::new(stage);
        assert_eq!(f.invoke().unwrap_err().code, "save_status_unknown");
        assert_eq!(f.client.recovery.borrow().len(), 1);
        assert_eq!(
            f.owner.0.lock().unwrap().commands,
            ["allocation", "reconcile"]
        );
    }
    let f = Fixture::new(NativeResultStage::Refused);
    assert_eq!(f.invoke().unwrap_err().code, "native_request_refused");
    assert!(f.client.recovery.borrow().is_empty());
}
#[test]
fn fresh_memory_work_is_explicitly_staged_and_cancellation_precedes_ports() {
    let f = Fixture::new(NativeResultStage::Registered);
    f.owner.0.lock().unwrap().allocation = None;
    assert_eq!(f.invoke().unwrap_err().code, "native_recovery_pending");
    assert_eq!(f.owner.0.lock().unwrap().commands, ["allocation"]);
    let token = Cancellation::default();
    token.cancel();
    let before = f.client.reads.get();
    assert!(
        invoke(
            &f.client,
            &f.owner,
            &f.record,
            f.provenance.clone(),
            f.captured.clone(),
            "remember_memory",
            f.args.clone(),
            token,
            None
        )
        .is_err()
    );
    assert_eq!(f.client.reads.get(), before);
}

#[test]
fn memory_receipt_subject_and_workspace_refuse_before_delegate_lookup() {
    for mutation in 0..4 {
        let mut f = Fixture::new(NativeResultStage::Registered);
        match mutation {
            0 => f.client.subject.subject.bot = BotId::mint(),
            1 => f.client.subject.profile = koru_ai_core::ProfileId::from_uuid(Uuid::nil()),
            2 => f.client.subject.policy_revision = 0,
            3 => {
                f.record.grants.insert(WorkspaceId::new());
            }
            _ => unreachable!(),
        }
        assert_eq!(f.invoke().unwrap_err().code, "permission_denied");
        assert!(f.owner.0.lock().unwrap().commands.is_empty());
    }
}
#[test]
fn memory_write_gate_catalog_and_complete_response_remain_bounded() {
    available().unwrap();
    for name in ["remember_memory", "correct_memory", "forget_memory"] {
        assert!(
            super::super::super::definitions()
                .iter()
                .any(|t| t.name == name)
        );
    }
    for tool in definitions() {
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert_eq!(
            tool.annotations.as_ref().unwrap().read_only_hint,
            Some(false)
        );
    }
    let catalog = super::super::super::definitions();
    // The four document tools, two Memory reads, three Memory writes and the
    // four own-Bot Routine tools (added in ef149885), in any order.
    let mut names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    let mut expected = [
        "search_documents",
        "read_document",
        "create_document",
        "edit_document",
        "list_memory",
        "read_memory",
        "remember_memory",
        "correct_memory",
        "forget_memory",
        "create_routine",
        "read_routine_creation",
        "retry_routine_creation",
        "list_routines",
    ];
    expected.sort_unstable();
    assert_eq!(names, expected);
    eprintln!(
        "Complete native catalog: {} serialized bytes",
        serde_json::to_vec(&json!({"tools":catalog})).unwrap().len()
    );
    let response = result(Ok(json!({"untrusted":"\"\\\n".repeat(16000)})));
    assert_eq!(response.is_error, Some(true));
    assert!(serde_json::to_vec(&response).unwrap().len() <= BOT_MEMORY_RESPONSE_BYTES);
}
