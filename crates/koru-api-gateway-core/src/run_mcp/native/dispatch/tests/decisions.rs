// SPDX-License-Identifier: MPL-2.0

//! The native edit path's decision branch (plan M4 P4.2, slice F), driven
//! through `invoke` with the in-memory Store as the real effect executor.
//!
//! **Scripted delegate double.** `DecisionOwner` answers the decision
//! commands the way the runtime and ledger answer them in
//! `koru_delegate_runtime` `orchestrator::decisions::edit::tests` (exact
//! replay before fresh checks, typed precommit refusals, one atomic consume).
//! It is not the real actor, IPC16 transport, ledger, `GatewayState`
//! bootstrap, AI policy worker or provider. No disk, network or telemetry.
use super::*;
use crate::run_mcp::native::decision::{DecisionPacing, EditDecisions};
use koru_delegate_api::bots::{BotBootScan, DecisionEditProposal, DecisionReceiptView};
use koru_delegate_core::decisions::*;
use std::time::Duration;

const PACING: DecisionPacing = DecisionPacing {
    replays: 3,
    first: Duration::ZERO,
    ceiling: Duration::ZERO,
};
const NOW_MS: i64 = 1_700_000_000_000;

fn r(value: u64) -> DecisionRevision {
    DecisionRevision::new(value).unwrap()
}
fn refusal(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation was refused.", false)
}

struct Script {
    fence: BotExecutionFence,
    captured: CapturedBotPolicy,
    boot: DelegateBootId,
    human: ProfileId,
    submission: BotRequestId,
    view: Option<DecisionReceiptView>,
    created: usize,
    /// Replays answered before the initial projection is acknowledged.
    acknowledge_after: usize,
    replays: usize,
    lose_created_reply: bool,
    refuse_fresh: Option<&'static str>,
    publication_pending: bool,
    /// Hide a retained intent from the native lookup (publication pending).
    hide_intent: bool,
    consumer: Option<BotExecutionFence>,
    intent: Option<NativeResultIntentView>,
    lose_consume_reply: bool,
    refuse_consume: Option<&'static str>,
    consumes: usize,
    leases: usize,
    ordinary_refusal: bool,
    /// Answer a receipt as the real owner does while the decision holds the
    /// Run document: `Verified` with the exact evidence, registration owed.
    hold_registration: bool,
    /// Answer a receipt with evidence other than what was sent.
    alter_receipt: bool,
    log: Vec<&'static str>,
    proposals: Vec<DecisionEditProposal>,
    hello_gate: Option<std::sync::mpsc::Receiver<()>>,
}

struct DecisionOwner(Mutex<Script>);

impl DecisionOwner {
    fn new(f: &Fixture) -> Self {
        Self(Mutex::new(Script {
            fence: f.owner.fence.clone(),
            captured: f.owner.policy.clone(),
            boot: DelegateBootId::mint(),
            human: serde_json::from_value(json!(Uuid::new_v4())).unwrap(),
            submission: f.provenance.submission,
            view: None,
            created: 0,
            acknowledge_after: 1,
            replays: 0,
            lose_created_reply: false,
            refuse_fresh: None,
            publication_pending: false,
            hide_intent: false,
            consumer: None,
            intent: None,
            lose_consume_reply: false,
            refuse_consume: None,
            consumes: 0,
            leases: 0,
            ordinary_refusal: false,
            hold_registration: false,
            alter_receipt: false,
            log: Vec::new(),
            proposals: Vec::new(),
            hello_gate: None,
        }))
    }
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.0.lock().unwrap()
    }
}

impl Script {
    fn view_for(
        &self,
        fence: &BotExecutionFence,
        edit: &DecisionEditProposal,
        proof: &FreshBotPolicyProof,
    ) -> DecisionReceiptView {
        let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
            version: DecisionVersionV1,
            owner: fence.owner,
            bot: fence.bot,
            run: fence.run,
            origin_attempt: fence.attempt,
            origin_generation: fence.generation,
            origin_session: fence.session,
            origin_turn: fence.turn.unwrap(),
            submission: self.submission,
            routine: None,
            human_profile: self.human,
            workspace: self.captured.workspace,
            target: edit.target,
            action: edit.action.clone(),
            operation_id: edit.operation_id,
            edit_fingerprint: edit.edit_fingerprint,
            expected_revision: edit.expected_revision.clone(),
            payload_version: DecisionVersionV1,
            payload: edit.payload.clone(),
            captured_policy: self.captured.clone(),
            global_policy: DecisionGlobalPolicy {
                key: proof.global.key.clone(),
                config_revision: r(proof.global.config_revision),
                policy_revision: r(proof.global.policy_revision),
                digest: DecisionDigest::from_bytes([7; 32]),
            },
            routine_policy: None,
        });
        let binding_digest = binding.digest().unwrap();
        DecisionReceiptView {
            receipt: DecisionReceipt {
                decision: DecisionId::mint(),
                binding,
                binding_digest,
                deadline: DecisionDeadline {
                    created_boot: self.boot,
                    created_at_ms: NOW_MS,
                    deadline_at_ms: NOW_MS + DECISION_WAIT_MILLIS,
                    native_deadline_at_ms: None,
                },
                state: DecisionState {
                    kind: DecisionKind::KoruEdit,
                    authorization: DecisionAuthorization::Pending,
                    phase: DecisionPhase::Publishing,
                    revision: r(1),
                },
                answer: None,
                consumed_at_ms: None,
                closure: None,
                last_observed_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
            },
            wait: BotWaitId::mint(),
            wait_phase: BotWaitPhase::Fenced,
            wait_revision: r(1),
            native_response: None,
            resume: None,
        }
    }

    fn prepare(
        &mut self,
        fence: BotExecutionFence,
        edit: DecisionEditProposal,
        proof: FreshBotPolicyProof,
        boot: DelegateBootId,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert_eq!(boot, self.boot, "the probed delegate boot");
        self.proposals.push(edit.clone());
        if self.view.is_none() {
            if let Some(code) = self.refuse_fresh {
                return Err(refusal(code));
            }
            let view = self.view_for(&fence, &edit, &proof);
            self.view = Some(view.clone());
            self.created += 1;
            if std::mem::take(&mut self.lose_created_reply) {
                // Committed, then the reply was lost.
                return Err(koru_delegate_api::Failure::unavailable());
            }
            return Ok(BotReply::Decision { decision: view });
        }
        let view = self.view.as_mut().expect("a retained decision");
        let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
            unreachable!()
        };
        if binding.operation_id != edit.operation_id {
            return Err(refusal("bot_decision_owned"));
        }
        if binding.payload != edit.payload
            || binding.edit_fingerprint != edit.edit_fingerprint
            || binding.expected_revision != edit.expected_revision
            || binding.target != edit.target
        {
            return Err(refusal("bot_conflict"));
        }
        self.replays += 1;
        let state = &mut view.receipt.state;
        if state.authorization == DecisionAuthorization::Pending
            && state.phase == DecisionPhase::Publishing
            && self.replays >= self.acknowledge_after
        {
            state.phase = DecisionPhase::Holding;
            state.revision = r(state.revision.get() + 1);
        }
        Ok(BotReply::Decision {
            decision: view.clone(),
        })
    }

    /// The person allowed it and the recorded consumer's input was
    /// acknowledged: Granted/AwaitRetry with the acknowledged resume.
    fn grant(&mut self, consumer: BotExecutionFence) {
        let view = self.view.as_mut().expect("a prepared decision");
        let receipt = &mut view.receipt;
        let human = match &receipt.binding {
            DecisionBindingV1::KoruEdit(binding) => binding.human_profile,
            _ => unreachable!(),
        };
        receipt.answer = Some(DecisionAnswerReceipt {
            decision: receipt.decision,
            request: DecisionAnswerId::mint(),
            expected_revision: r(3),
            committed_revision: r(4),
            binding_digest: receipt.binding_digest,
            human_profile: human,
            choice: DecisionAnswerChoice::AllowOnce {},
            answered_at_ms: NOW_MS + 1,
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        });
        receipt.state = DecisionState {
            kind: DecisionKind::KoruEdit,
            authorization: DecisionAuthorization::Granted,
            phase: DecisionPhase::AwaitRetry,
            revision: r(6),
        };
        view.wait_phase = BotWaitPhase::ResumeReserved;
        view.resume = Some(DecisionResume {
            decision: receipt.decision,
            origin: receipt.binding.origin(),
            consumer: DecisionConsumer::RecordedSuccessor {
                attempt: consumer.attempt,
                generation: consumer.generation,
                creation_key: Uuid::new_v4().to_string().parse().unwrap(),
                session: Some(consumer.session),
            },
            input_key: Uuid::new_v4().to_string().parse().unwrap(),
            input_fingerprint: [5; 32],
            state: DecisionResumeState::InputAcknowledged {
                acknowledgment: DecisionInputAcknowledgment {
                    turn: consumer.turn.unwrap(),
                    receipt_digest: DecisionDigest::from_bytes([6; 32]),
                },
            },
        });
        self.consumer = Some(consumer.clone());
        self.fence = consumer;
    }

    fn consume(
        &mut self,
        fence: BotExecutionFence,
        request: PrepareNativeResult,
        payload: DecisionPayload,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        self.consumes += 1;
        if let Some(code) = self.refuse_consume {
            return Err(refusal(code));
        }
        let view = self.view.as_ref().expect("a decision");
        let DecisionBindingV1::KoruEdit(binding) = &view.receipt.binding else {
            unreachable!()
        };
        assert_eq!(Some(&fence), self.consumer.as_ref(), "only the consumer");
        if payload != binding.payload
            || request.request_fingerprint != binding.edit_fingerprint
            || request.expected_revision.as_deref() != Some(binding.expected_revision.as_slice())
            || request.target != Some(binding.target)
            || request.config_revision != binding.global_policy.config_revision.get()
            || request.policy_revision != binding.global_policy.policy_revision.get()
            || request.key.turn != fence.turn.unwrap()
        {
            return Err(refusal("bot_conflict"));
        }
        let execute = self.intent.is_none();
        if execute {
            self.leases += 1;
            self.intent = Some(NativeResultIntentView {
                purpose: NativeEffectPurpose::Result {},
                key: request.key,
                kind: NativeResultOperation::EditPage,
                destination: request.destination,
                request_fingerprint: request.request_fingerprint,
                target: binding.target,
                store_request: Uuid::new_v4(),
                stage: NativeResultStage::Prepared,
                verified: None,
                disposition: NativeResultDisposition::ReconcileOnly,
            });
        }
        let mut intent = self.intent.clone().unwrap();
        intent.disposition = if execute {
            NativeResultDisposition::Execute
        } else {
            NativeResultDisposition::ReconcileOnly
        };
        if execute && std::mem::take(&mut self.lose_consume_reply) {
            return Err(koru_delegate_api::Failure::unavailable());
        }
        Ok(BotReply::NativeResult { intent })
    }
}

impl BotDelegatePort for DecisionOwner {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        if matches!(command, BotCommand::Hello { .. }) {
            let gate = self.script().hello_gate.take();
            if let Some(gate) = gate {
                gate.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }
        let mut s = self.script();
        match command {
            BotCommand::Reconcile { key } => {
                s.log.push("reconcile");
                Ok(BotReply::Admission {
                    admission: BotAdmissionView {
                        key,
                        run: s.fence.run,
                        state: BotAdmissionState::Executing,
                        queue_sequence: 1,
                        fence: Some(s.fence.clone()),
                    },
                })
            }
            BotCommand::ReconcileNativeResult { key } => {
                s.log.push("native_lookup");
                match &s.intent {
                    Some(intent) if !s.hide_intent => {
                        assert_eq!(intent.key, key);
                        Ok(BotReply::NativeResult {
                            intent: intent.clone(),
                        })
                    }
                    _ if s.publication_pending || s.hide_intent => {
                        Err(koru_delegate_api::Failure::new(
                            "publication_pending",
                            "The Run publication is still in progress.",
                            true,
                        ))
                    }
                    _ => Ok(BotReply::Missing),
                }
            }
            BotCommand::Hello { owner, scan } => {
                s.log.push("hello");
                assert_eq!(owner, s.fence.owner);
                assert!(matches!(scan, BotBootScan::Probe {}));
                Ok(BotReply::Hello {
                    hello: BotHello {
                        boot: s.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
            BotCommand::SynchronizePolicy { barrier } => {
                s.log.push("sync");
                Ok(BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: BotPolicyBarrierState::Effective,
                    },
                })
            }
            BotCommand::EffectFence { fence } => {
                s.log.push("effect_fence");
                Ok(BotReply::EffectFence {
                    fence,
                    policy: s.captured.clone(),
                })
            }
            BotCommand::PrepareEditDecision {
                fence,
                edit,
                proof,
                boot,
            } => {
                s.log.push("prepare_decision");
                s.prepare(fence, edit, proof, boot)
            }
            BotCommand::ConsumeDecisionEdit {
                fence,
                request,
                payload,
                boot,
                ..
            } => {
                s.log.push("consume");
                assert_eq!(boot, s.boot);
                s.consume(fence, request, payload)
            }
            BotCommand::ConfirmNativeResult { request } => {
                s.log.push("confirm");
                let (hold, alter) = (s.hold_registration, s.alter_receipt);
                let intent = s.intent.as_mut().unwrap();
                assert_eq!(intent.key, request.key);
                match request.observation {
                    NativeResultObservation::Durable { evidence } => {
                        intent.verified = Some(evidence);
                        if alter {
                            let mut other = intent.clone();
                            intent.stage = NativeResultStage::Verified;
                            other.stage = NativeResultStage::Verified;
                            if let Some(evidence) = other.verified.as_mut() {
                                evidence.title.push_str(" (other)");
                            }
                            return Ok(BotReply::NativeResult { intent: other });
                        }
                        if hold {
                            intent.stage = NativeResultStage::Verified;
                        } else {
                            intent.stage = NativeResultStage::Registered;
                            intent.disposition = NativeResultDisposition::AlreadyRegistered;
                        }
                    }
                    NativeResultObservation::NotApplied { .. } => {
                        intent.stage = NativeResultStage::Refused
                    }
                    _ => intent.stage = NativeResultStage::Unconfirmed,
                }
                Ok(BotReply::NativeResult {
                    intent: intent.clone(),
                })
            }
            BotCommand::PrepareNativeResult { .. } => {
                s.log.push("ordinary_prepare");
                if s.ordinary_refusal {
                    // The existing lease-time selected-page refusal.
                    return Err(refusal("bot_fenced"));
                }
                Err(refusal("bot_native_unavailable"))
            }
            // M6: the child-wait fence names the open wait's requests when the
            // owner can say them; this owner cannot.
            BotCommand::ReadChildRequests { .. } => {
                s.log.push("read_children");
                Err(koru_delegate_api::Failure::unavailable())
            }
            // A Routine Run's ordinary path also synchronizes its grant.
            BotCommand::SynchronizeRoutinePolicy { barrier } => {
                s.log.push("routine_sync");
                Ok(BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: koru_delegate_core::BotPolicyBarrierState::Effective,
                })
            }
            _ => panic!("unexpected command"),
        }
    }
}

/// A page in the Run's home created through the ordinary path, then an
/// edit of it (with its own operation) that the Bot's selected-page access
/// does not grant.
fn denied_edit(f: &Fixture) -> Value {
    let created = f.call("create_document", f.arguments()).unwrap();
    let document = created["document_id"].clone();
    let read = f
        .call(
            "read_document",
            json!({"workspace_id":f.record.workspace,"document_id":document}),
        )
        .unwrap();
    json!({"workspace_id":f.record.workspace,"document_id":document,
        "operation_id":Uuid::new_v4(),"base_revision":read["revision"],
        "operations":[{"op":"insert","parent_id":read["root_node_id"],"after_node_id":null,"kind":"paragraph","text":"Needs approval"}]})
}

fn edit(f: &Fixture, owner: &DecisionOwner, arguments: Value) -> Result<Value, ToolFailure> {
    edit_with(f, &f.policy, owner, arguments, EditDecisions::Open(PACING))
}

fn edit_with(
    f: &Fixture,
    policy: &dyn PolicySource,
    owner: &DecisionOwner,
    arguments: Value,
    decisions: EditDecisions,
) -> Result<Value, ToolFailure> {
    invoke(
        &f.memory.client(),
        policy,
        owner,
        &f.record,
        f.provenance.clone(),
        f.owner.policy.clone(),
        "edit_document",
        arguments,
        decisions,
        false,
    )
}

fn text_count(f: &Fixture, arguments: &Value, text: &str) -> usize {
    let read = f
        .call(
            "read_document",
            json!({"workspace_id":f.record.workspace,"document_id":arguments["document_id"]}),
        )
        .unwrap();
    read.to_string().matches(text).count()
}

fn operation_of(arguments: &Value) -> Value {
    arguments["operation_id"].clone()
}

#[test]
fn selected_page_denial_prepares_one_decision_and_is_pending_only_after_the_initial_projection() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().acknowledge_after = 2;
    let answer = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(answer.code, "permission_pending");
    let details = answer.details.unwrap();
    assert_eq!(details["operation_id"], operation_of(&arguments));
    assert_eq!(details["phase"], "holding");
    let s = owner.script();
    assert_eq!(
        details["decision_id"],
        json!(s.view.as_ref().unwrap().receipt.decision)
    );
    assert_eq!(s.created, 1, "one question");
    // Retained lookup, then the decision; no native intent, lease or Store
    // request was asked for, and nothing was written.
    assert_eq!(
        s.log,
        vec![
            "reconcile",
            "native_lookup",
            "hello",
            "prepare_decision",
            "prepare_decision",
            "prepare_decision"
        ]
    );
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);
}

#[test]
fn an_unacknowledged_initial_projection_answers_same_operation_status_unknown_within_the_bound() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().acknowledge_after = usize::MAX;
    let answer = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(answer.code, "decision_status_unknown");
    assert_eq!(
        answer.details.unwrap()["operation_id"],
        operation_of(&arguments)
    );
    let s = owner.script();
    // One preparation plus exactly the bounded replays.
    assert_eq!(s.proposals.len(), 1 + usize::from(PACING.replays));
    assert_eq!(s.created, 1);
}

#[test]
fn postcommit_lost_reply_is_status_unknown_and_the_exact_retry_reconciles_one_question() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().lose_created_reply = true;
    let lost = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(lost.code, "decision_status_unknown");
    assert_eq!(
        lost.details.unwrap()["operation_id"],
        operation_of(&arguments)
    );
    // The decision now owns the Run: the native lookup cannot prove absence.
    owner.script().publication_pending = true;
    let retried = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(retried.code, "permission_pending");
    let s = owner.script();
    assert_eq!(s.created, 1, "the retry replayed the retained decision");
    assert_eq!(
        retried.details.unwrap()["decision_id"],
        json!(s.view.as_ref().unwrap().receipt.decision)
    );
    assert!(!s.log.contains(&"ordinary_prepare"));
}

#[test]
fn exact_denied_body_and_version_vector_are_bound_and_a_changed_retry_is_a_conflict() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    // What the binding retains is the shared builder's own description of
    // the denied edit, and its canonical arguments without operation_id.
    let mut body = arguments.clone();
    body.as_object_mut().unwrap().remove("operation_id");
    let prepared = koru_ai_server::native_store_backend(f.memory.client(), f.authority())
        .prepare_native_write("edit_document", body.clone())
        .unwrap();
    let description = prepared.description();
    {
        let s = owner.script();
        let proposal = &s.proposals[0];
        assert_eq!(Some(proposal.target), description.document);
        assert_eq!(
            proposal.edit_fingerprint,
            fingerprint(&description.fingerprint).unwrap()
        );
        assert_eq!(
            Some(proposal.expected_revision.as_slice()),
            description.expected_revision.as_deref()
        );
        assert_eq!(
            proposal.payload.as_slice(),
            serde_json::to_vec(&body).unwrap()
        );
        assert!(!String::from_utf8_lossy(proposal.payload.as_slice()).contains("operation_id"));
    }
    // The same operation with a different body is never a second question
    // and never reaches the lease path.
    owner.script().publication_pending = true;
    let mut changed = arguments.clone();
    changed["operations"][0]["text"] = json!("Something else");
    assert_eq!(
        edit(&f, &owner, changed).unwrap_err().code,
        "native_operation_conflict"
    );
    let s = owner.script();
    assert_eq!(s.created, 1);
    assert!(!s.log.contains(&"ordinary_prepare"));
}

#[test]
fn non_grant_refusals_keep_the_existing_lease_path_and_send_no_decision() {
    // The gate is closed in this build: a denied edit takes the ordinary path.
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().ordinary_refusal = true;
    assert_eq!(
        edit_with(
            &f,
            &f.policy,
            &owner,
            arguments.clone(),
            EditDecisions::production()
        )
        .unwrap_err()
        .code,
        "native_owner_unavailable"
    );
    assert!(!super::super::super::decision::EDIT_DECISIONS_AVAILABLE);
    let ordinary = vec![
        "reconcile",
        "native_lookup",
        "sync",
        "effect_fence",
        "ordinary_prepare",
    ];
    assert_eq!(owner.script().log, ordinary);

    // Granted pages and whole-workspace access are not denials.
    for grant in ["page", "workspace"] {
        let mut f = Fixture::new();
        let arguments = denied_edit(&f);
        let document: DocumentId =
            serde_json::from_value(arguments["document_id"].clone()).unwrap();
        let policy = &mut f.policy.evidence.proof.global.policy;
        match grant {
            "page" => {
                policy.selected_pages.insert(document);
            }
            _ => policy.access = BotAccess::Workspace,
        }
        f.owner.policy = policy.clone();
        let owner = DecisionOwner::new(&f);
        owner.script().ordinary_refusal = true;
        assert!(edit(&f, &owner, arguments).is_err());
        assert_eq!(owner.script().log, ordinary, "{grant}");
    }

    // Effects not allowed: never a decision; the existing refusal stands.
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    f.policy.evidence.proof.global.effects_allowed = false;
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments).unwrap_err().code,
        "native_policy_pending"
    );
    assert_eq!(owner.script().log, vec!["reconcile", "native_lookup"]);
}

#[test]
fn a_typed_owner_refusal_of_the_decision_falls_back_to_the_existing_refusal() {
    // e.g. a page this Run created (the owner's own predicate), the device
    // cap, or a successor that could never be launched.
    for code in ["bot_fenced", "bot_capacity", "bot_successor_unsupported"] {
        let f = Fixture::new();
        let arguments = denied_edit(&f);
        let owner = DecisionOwner::new(&f);
        {
            let mut s = owner.script();
            s.refuse_fresh = Some(code);
            s.ordinary_refusal = true;
        }
        assert_eq!(
            edit(&f, &owner, arguments.clone()).unwrap_err().code,
            "native_owner_unavailable",
            "{code}"
        );
        let s = owner.script();
        assert_eq!(
            s.log,
            vec![
                "reconcile",
                "native_lookup",
                "hello",
                "prepare_decision",
                "sync",
                "effect_fence",
                "prepare_decision",
                "ordinary_prepare"
            ],
            "{code}"
        );
        assert_eq!(s.created, 0);
        drop(s);
        assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);
    }
    // Another decision owns the Run: in progress, never a second question
    // and never the lease path.
    let f = Fixture::new();
    let first = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, first.clone()).unwrap_err().code,
        "permission_pending"
    );
    owner.script().publication_pending = true;
    // New work: another operation (here on the same page, other text).
    let mut second = first;
    second["operation_id"] = json!(Uuid::new_v4());
    second["operations"][0]["text"] = json!("Other work");
    let answer = edit(&f, &owner, second.clone()).unwrap_err();
    assert_eq!(answer.code, "decision_in_progress");
    assert_eq!(
        answer.details.unwrap()["operation_id"],
        operation_of(&second)
    );
    let s = owner.script();
    assert_eq!(s.created, 1);
    assert!(!s.log.contains(&"ordinary_prepare"));
}

struct Unavailable;
impl PolicySource for Unavailable {
    fn current(&self, _: NativePolicyRequest) -> Result<NativePolicyEvidence, ToolFailure> {
        Err(failure("native_policy_unavailable"))
    }
    fn settled(&self) {}
    fn record_decision(&self, _: &DecisionReceiptView) -> Result<(), ToolFailure> {
        panic!("no decision without a current proof")
    }
}

#[test]
fn stale_human_or_policy_refuses_before_any_decision_or_effect() {
    // The current human/Store proof is unavailable: refused before anything.
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit_with(
            &f,
            &Unavailable,
            &owner,
            arguments.clone(),
            EditDecisions::Open(PACING)
        )
        .unwrap_err()
        .code,
        "native_policy_unavailable"
    );
    assert!(owner.script().log.is_empty());
    // A current proof older than the Run's captured revision is stale.
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    f.provenance.config_revision = 2;
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments).unwrap_err().code,
        "native_policy_unavailable"
    );
    assert!(owner.script().log.is_empty());
    // The consumer's policy or human changed since the answer: the owner
    // refuses the consume and no Store effect runs.
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    let consumer = successor(&mut f, &owner);
    {
        let mut s = owner.script();
        s.publication_pending = true;
        s.refuse_consume = Some("bot_fenced");
        s.grant(consumer);
    }
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "native_execution_unavailable"
    );
    let s = owner.script();
    assert_eq!((s.consumes, s.leases), (1, 0));
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);
}

/// The recorded successor's acknowledged fence; the Run bearer is now the
/// successor generation's.
fn successor(f: &mut Fixture, owner: &DecisionOwner) -> BotExecutionFence {
    let origin = owner.script().fence.clone();
    let consumer = BotExecutionFence {
        attempt: RunAttemptId::mint(),
        generation: origin.generation.next(),
        session: SessionId::mint(),
        turn: Some(AgentTurnId::mint()),
        ..origin
    };
    f.record.generation = consumer.generation;
    consumer
}

#[test]
fn single_consumption_the_acknowledged_consumer_runs_the_store_effect_once() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    let consumer = successor(&mut f, &owner);
    {
        let mut s = owner.script();
        s.publication_pending = true;
        s.grant(consumer);
    }
    let saved = edit(&f, &owner, arguments.clone()).unwrap();
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved["operation_id"], operation_of(&arguments));
    // The read shows one paragraph's text in more than one field; whatever
    // that count is after the one effect, a retry must not change it.
    let once = text_count(&f, &arguments, "Needs approval");
    assert!(once > 0, "the approved edit was applied");
    // A retry while the decision still owns the Run: the consume replays
    // (no Execute) and the retained receipt answers. Never a second effect.
    owner.script().hide_intent = true;
    assert_eq!(edit(&f, &owner, arguments.clone()).unwrap(), saved);
    // After publication: the ordinary retained lookup answers directly.
    {
        let mut s = owner.script();
        s.hide_intent = false;
        s.publication_pending = false;
    }
    assert_eq!(edit(&f, &owner, arguments.clone()).unwrap(), saved);
    let s = owner.script();
    assert_eq!((s.consumes, s.leases), (2, 1));
    assert!(!s.log.contains(&"ordinary_prepare"));
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), once);
}

#[test]
fn one_use_control_a_lost_consume_reply_never_runs_the_effect_blind() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    let consumer = successor(&mut f, &owner);
    {
        let mut s = owner.script();
        s.publication_pending = true;
        s.hide_intent = true;
        s.lose_consume_reply = true;
        s.grant(consumer);
    }
    // The one lease was committed but its reply was lost: unknown, no effect.
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "save_status_unknown"
    );
    // The retry's consume replays without Execute; the Store holds nothing
    // for the original request, so the outcome stays unknown and nothing
    // runs: the one-use approval is never licensed twice.
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "save_status_unknown"
    );
    let s = owner.script();
    assert_eq!((s.consumes, s.leases), (2, 1));
    assert!(!s.log.contains(&"confirm"));
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_caller_drop_does_not_cancel_a_committed_decision_and_the_retry_reconciles() {
    let f = std::sync::Arc::new(Fixture::new());
    let arguments = denied_edit(&f);
    let owner = std::sync::Arc::new(DecisionOwner::new(&f));
    let (go, gate) = std::sync::mpsc::sync_channel(1);
    let (done, finished) = std::sync::mpsc::sync_channel(1);
    owner.script().hello_gate = Some(gate);
    let task = {
        let (f, owner, arguments) = (f.clone(), owner.clone(), arguments.clone());
        tokio::task::spawn_blocking(move || {
            let answer = edit(&f, &owner, arguments);
            let _ = done.send(answer);
        })
    };
    // The caller (the HTTP future awaiting this blocking call) goes away
    // before the owner is reached.
    drop(task);
    go.send(()).unwrap();
    let first = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the blocking call ran to completion after its caller dropped");
    assert_eq!(first.unwrap_err().code, "permission_pending");
    owner.script().publication_pending = true;
    let retried = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(retried.code, "permission_pending");
    assert_eq!(owner.script().created, 1);
}

#[test]
fn creation_and_routine_tools_never_enter_the_edit_decision_path() {
    // O93: accepted Routine creation recovery is a different journal; this
    // path neither fences nor replaces it. Only `edit_document` can ask.
    let f = Fixture::new();
    let owner = DecisionOwner::new(&f);
    for tool in [
        "create_routine",
        "retry_routine_creation",
        "read_routine_creation",
        "list_routines",
    ] {
        assert_eq!(
            invoke(
                &f.memory.client(),
                &f.policy,
                &owner,
                &f.record,
                f.provenance.clone(),
                f.owner.policy.clone(),
                tool,
                json!({}),
                EditDecisions::Open(PACING),
                false,
            )
            .unwrap_err()
            .code,
            "unknown_tool"
        );
    }
    assert!(owner.script().log.is_empty());
    let created = invoke(
        &f.memory.client(),
        &f.policy,
        &owner,
        &f.record,
        f.provenance.clone(),
        f.owner.policy.clone(),
        "create_document",
        f.arguments(),
        EditDecisions::Open(PACING),
        false,
    );
    assert!(created.is_err());
    assert!(!owner.script().log.contains(&"prepare_decision"));
    assert!(!owner.script().log.contains(&"hello"));
}

fn recorded(f: &Fixture) -> Vec<DecisionReceiptView> {
    f.policy.recorded.lock().unwrap().clone()
}

#[test]
fn a_positive_prepare_records_each_exact_receipt_and_a_replay_records_the_same_one() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().acknowledge_after = 2;
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    // The fresh reply and both replays each passed the point once.
    let first = recorded(&f);
    assert_eq!(first.len(), 3);
    let current = owner.script().view.clone().unwrap();
    assert!(
        first
            .iter()
            .all(|view| view.receipt.decision == current.receipt.decision
                && view.receipt.binding == current.receipt.binding)
    );
    assert_eq!(first[0].receipt.state.phase, DecisionPhase::Publishing);
    assert_eq!(first.last(), Some(&current), "the exact receipt answered");
    // The exact retry's replay records the same receipt again.
    owner.script().publication_pending = true;
    assert_eq!(
        edit(&f, &owner, arguments).unwrap_err().code,
        "permission_pending"
    );
    let after = recorded(&f);
    assert_eq!(after.len(), 4);
    assert_eq!(after[3], owner.script().view.clone().unwrap());
}

#[test]
fn a_refused_or_unknown_prepare_records_nothing() {
    // A typed refusal: the ordinary path answers; nothing positive existed.
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    {
        let mut s = owner.script();
        s.refuse_fresh = Some("bot_fenced");
        s.ordinary_refusal = true;
    }
    assert!(edit(&f, &owner, arguments).is_err());
    assert!(recorded(&f).is_empty());
    // A lost reply after commit: unknown, and no receipt was seen.
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    owner.script().lose_created_reply = true;
    assert_eq!(
        edit(&f, &owner, arguments.clone()).unwrap_err().code,
        "decision_status_unknown"
    );
    assert!(recorded(&f).is_empty());
    // Another operation while that decision owns the Run: in progress.
    owner.script().publication_pending = true;
    let before = {
        assert_eq!(
            edit(&f, &owner, arguments.clone()).unwrap_err().code,
            "permission_pending"
        );
        recorded(&f).len()
    };
    let mut other = arguments;
    other["operation_id"] = json!(Uuid::new_v4());
    other["operations"][0]["text"] = json!("Other work");
    assert_eq!(
        edit(&f, &owner, other).unwrap_err().code,
        "decision_in_progress"
    );
    assert_eq!(recorded(&f).len(), before);
}

#[test]
fn a_failed_record_never_changes_the_decision_answer_and_the_next_replay_records_again() {
    let f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    f.policy.refuse_record.store(true, Ordering::SeqCst);
    let answer = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(
        answer.code, "permission_pending",
        "still the positive answer"
    );
    let attempts = recorded(&f).len();
    assert!(attempts > 0, "the record was attempted");
    // The AI can take it now: the exact retry's replay records it.
    f.policy.refuse_record.store(false, Ordering::SeqCst);
    owner.script().publication_pending = true;
    assert_eq!(
        edit(&f, &owner, arguments).unwrap_err().code,
        "permission_pending"
    );
    let after = recorded(&f);
    assert_eq!(after.len(), attempts + 1);
    assert_eq!(after.last(), owner.script().view.as_ref());
    assert_eq!(owner.script().created, 1);
}

// ---------------------------------------------------------------- O147 fence

fn tool_while(
    f: &Fixture,
    owner: &DecisionOwner,
    tool: &str,
    arguments: Value,
    decisions: EditDecisions,
    waiting: bool,
) -> Result<Value, ToolFailure> {
    invoke(
        &f.memory.client(),
        &f.policy,
        owner,
        &f.record,
        f.provenance.clone(),
        f.owner.policy.clone(),
        tool,
        arguments,
        decisions,
        waiting,
    )
}

#[test]
fn the_approved_edits_consume_still_succeeds_while_the_decision_waits() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        tool_while(
            &f,
            &owner,
            "edit_document",
            arguments.clone(),
            EditDecisions::Open(PACING),
            false
        )
        .unwrap_err()
        .code,
        "permission_pending"
    );
    let consumer = successor(&mut f, &owner);
    {
        let mut s = owner.script();
        s.publication_pending = true;
        s.grant(consumer);
    }
    // The Run classifies as waiting for the whole consume.
    let saved = tool_while(
        &f,
        &owner,
        "edit_document",
        arguments.clone(),
        EditDecisions::Open(PACING),
        true,
    )
    .unwrap();
    assert_eq!(saved["status"], "saved");
    assert!(text_count(&f, &arguments, "Needs approval") > 0);
    let s = owner.script();
    assert_eq!((s.consumes, s.leases), (1, 1));
    assert!(!s.log.contains(&"ordinary_prepare"));
}

#[test]
fn while_a_decision_waits_reads_pass_and_new_mutations_are_refused_typed() {
    let f = Fixture::new();
    let first = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    // Reads are unaffected.
    let read = tool_while(
        &f,
        &owner,
        "read_document",
        json!({"workspace_id":f.record.workspace,"document_id":first["document_id"]}),
        EditDecisions::Open(PACING),
        true,
    )
    .unwrap();
    assert!(read.get("revision").is_some());
    // A search is a read too: whatever it answers, it is never the fence.
    assert!(
        tool_while(
            &f,
            &owner,
            "search_documents",
            json!({"workspace_id":f.record.workspace,"query":"Native"}),
            EditDecisions::Open(PACING),
            true,
        )
        .err()
        .is_none_or(|error| error.code != "decision_in_progress")
    );
    assert!(
        owner.script().log.is_empty(),
        "a read asks the owner nothing"
    );
    // A new page is a new mutation: refused, typed, before any lease.
    let created = tool_while(
        &f,
        &owner,
        "create_document",
        f.arguments(),
        EditDecisions::Open(PACING),
        true,
    )
    .unwrap_err();
    assert_eq!(created.code, "decision_in_progress");
    assert!(created.details.unwrap()["operation_id"].is_string());
    // With decisions closed (this build), an edit is refused the same way.
    let edited = tool_while(
        &f,
        &owner,
        "edit_document",
        first.clone(),
        EditDecisions::Closed,
        true,
    )
    .unwrap_err();
    assert_eq!(edited.code, "decision_in_progress");
    // And with decisions open, an edit that the owner refuses as a decision
    // never falls back to a lease while the Run waits.
    owner.script().refuse_fresh = Some("bot_fenced");
    let refused = tool_while(
        &f,
        &owner,
        "edit_document",
        first,
        EditDecisions::Open(PACING),
        true,
    )
    .unwrap_err();
    assert_eq!(refused.code, "decision_in_progress");
    let s = owner.script();
    assert!(!s.log.contains(&"ordinary_prepare"));
    assert!(!s.log.contains(&"consume"));
}

/// M6 contract 4.6: the same Bot Run while it waits on another Bot.
#[test]
fn m6_tool_fence_during_a_child_wait_reads_pass_mutations_are_refused_and_no_decision_is_asked() {
    let f = Fixture::new();
    let denied = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    let child = |tool: &str, arguments: Value, decisions: EditDecisions| {
        invoke_for(
            &f.memory.client(),
            &f.policy,
            &owner,
            &f.record,
            f.provenance.clone(),
            f.owner.policy.clone(),
            tool,
            arguments,
            decisions,
            RunWait::Child,
        )
    };
    // A read passes and asks the owner nothing.
    let read = child(
        "read_document",
        json!({"workspace_id":f.record.workspace,"document_id":denied["document_id"]}),
        EditDecisions::Open(PACING),
    )
    .unwrap();
    assert!(read.get("revision").is_some());
    assert!(owner.script().log.is_empty());
    // A new page: refused with the child wait's code, keyed by its operation.
    let arguments = f.arguments();
    let created = child("create_document", arguments.clone(), EditDecisions::Closed).unwrap_err();
    assert_eq!(created.code, "child_wait_in_progress");
    assert_eq!(
        created.details,
        Some(json!({"operation_id": arguments["operation_id"]}))
    );
    // An edit that would have asked a decision, with decisions open: no
    // decision is prepared; it gets the plain refusal of the wait.
    let edited = child("edit_document", denied.clone(), EditDecisions::Open(PACING)).unwrap_err();
    assert_eq!(edited.code, "child_wait_in_progress");
    let s = owner.script();
    assert!(!s.log.contains(&"prepare_decision"), "{:?}", s.log);
    assert!(!s.log.contains(&"hello"));
    assert!(!s.log.contains(&"ordinary_prepare"));
    assert!(!s.log.contains(&"effect_fence"));
    assert_eq!(s.log.iter().filter(|e| **e == "read_children").count(), 2);
    drop(s);
    // The same edit while a decision waits keeps today's answer.
    assert_eq!(
        tool_while(
            &f,
            &owner,
            "edit_document",
            denied,
            EditDecisions::Closed,
            true
        )
        .unwrap_err()
        .code,
        "decision_in_progress"
    );
    // The open-child-wait check is the decision builder's own.
    assert!(matches!(
        EditDecisions::Open(PACING).for_wait(RunWait::Child),
        EditDecisions::Closed
    ));
    assert!(matches!(
        EditDecisions::Open(PACING).for_wait(RunWait::Decision),
        EditDecisions::Open(_)
    ));
}

#[test]
fn the_fence_lifts_when_the_wait_settles_or_closes() {
    let f = Fixture::new();
    let owner = DecisionOwner::new(&f);
    assert_eq!(
        tool_while(
            &f,
            &owner,
            "create_document",
            f.arguments(),
            EditDecisions::Closed,
            true
        )
        .unwrap_err()
        .code,
        "decision_in_progress"
    );
    assert!(!owner.script().log.contains(&"ordinary_prepare"));
    // The classification no longer reports the wait: the ordinary path runs
    // again (its scripted lease refusal here is the existing one).
    owner.script().ordinary_refusal = true;
    assert_eq!(
        tool_while(
            &f,
            &owner,
            "create_document",
            f.arguments(),
            EditDecisions::Closed,
            false
        )
        .unwrap_err()
        .code,
        "native_owner_unavailable"
    );
    assert!(owner.script().log.contains(&"ordinary_prepare"));
}

// ---------------------------------------------------------------- O152 save

/// A granted decision whose consumer is now the Run bearer; the owner holds
/// the Run document (as while the decision's wait is open) and answers a
/// receipt `Verified`.
fn held(f: &mut Fixture, owner: &DecisionOwner, arguments: &Value) {
    assert_eq!(
        edit(f, owner, arguments.clone()).unwrap_err().code,
        "permission_pending"
    );
    let consumer = successor(f, owner);
    let mut s = owner.script();
    s.publication_pending = true;
    s.hold_registration = true;
    s.grant(consumer);
}

fn waiting_edit(
    f: &Fixture,
    owner: &DecisionOwner,
    arguments: &Value,
    decisions: EditDecisions,
) -> Result<Value, ToolFailure> {
    tool_while(
        f,
        owner,
        "edit_document",
        arguments.clone(),
        decisions,
        true,
    )
}

#[test]
fn the_approved_edits_verified_save_answers_saved_and_its_exact_retry_the_same_reference() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    held(&mut f, &owner, &arguments);
    // The consumer's consume runs the Store effect once; the owner records
    // it Verified with exactly the evidence sent: a positive save.
    let saved = waiting_edit(&f, &owner, &arguments, EditDecisions::Open(PACING)).unwrap();
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved["operation_id"], operation_of(&arguments));
    let once = text_count(&f, &arguments, "Needs approval");
    assert!(once > 0, "the approved edit was applied");
    assert_eq!(
        owner.script().intent.as_ref().unwrap().stage,
        NativeResultStage::Verified
    );
    // The model's exact retry finds the retained intent; the decision's
    // read-only replay names it as its consumed edit, so the same Verified
    // save answers with the same reference, without waiting for the fence.
    let before = owner.script().log.len();
    let retried = waiting_edit(&f, &owner, &arguments, EditDecisions::Open(PACING)).unwrap();
    assert_eq!(retried, saved);
    let s = owner.script();
    assert_eq!(
        s.log[before..],
        [
            "reconcile",
            "native_lookup",
            "hello",
            "prepare_decision",
            "confirm"
        ]
    );
    assert_eq!((s.consumes, s.leases), (1, 1), "never a second license");
    assert!(!s.log.contains(&"ordinary_prepare"));
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), once);
}

#[test]
fn a_verified_save_with_other_evidence_stays_unknown() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    held(&mut f, &owner, &arguments);
    owner.script().alter_receipt = true;
    assert_eq!(
        waiting_edit(&f, &owner, &arguments, EditDecisions::Open(PACING))
            .unwrap_err()
            .code,
        "save_status_unknown"
    );
    assert_eq!(
        waiting_edit(&f, &owner, &arguments, EditDecisions::Open(PACING))
            .unwrap_err()
            .code,
        "save_status_unknown"
    );
    assert_eq!(owner.script().leases, 1);
}

/// Outside a proven decision context a `Verified` reply keeps today's
/// answer: not waiting, or with edit decisions closed (no decision replay).
#[test]
fn a_verified_reply_without_the_decisions_replay_keeps_todays_answer() {
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    let owner = DecisionOwner::new(&f);
    held(&mut f, &owner, &arguments);
    assert_eq!(
        waiting_edit(&f, &owner, &arguments, EditDecisions::Open(PACING)).unwrap()["status"],
        "saved"
    );
    let before = owner.script().log.len();
    assert_eq!(
        tool_while(
            &f,
            &owner,
            "edit_document",
            arguments.clone(),
            EditDecisions::Open(PACING),
            false
        )
        .unwrap_err()
        .code,
        "save_status_unknown"
    );
    assert_eq!(
        waiting_edit(&f, &owner, &arguments, EditDecisions::Closed)
            .unwrap_err()
            .code,
        "save_status_unknown"
    );
    let s = owner.script();
    assert!(
        !s.log[before..].contains(&"prepare_decision"),
        "no decision replay outside its context"
    );
    assert_eq!((s.consumes, s.leases), (1, 1));
}

/// Make the fixture's Run a Routine Run whose own grant (captured and
/// current) is the fixture's policy: no selected page.
fn as_routine(f: &mut Fixture) {
    use koru_delegate_core::{
        RoutineDefinitionIdentity, RoutineDigest32, RoutinePolicyBarrier, RoutineProvenance,
        ScheduleId,
    };
    let schedule = ScheduleId::from_uuid(Uuid::new_v4());
    f.provenance.routine = Some(RoutineProvenance {
        schedule,
        occurrence_ms: 900,
        definition: RoutineDefinitionIdentity {
            revision: 2,
            digest: RoutineDigest32::from_bytes([5; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    });
    f.policy.evidence.proof.routine = Some(RoutinePolicyBarrier {
        owner: f.provenance.owner,
        bot: f.provenance.bot,
        schedule,
        request: Uuid::new_v4().try_into().unwrap(),
        grant_revision: 1,
        policy: f.owner.policy.clone(),
        effects_allowed: true,
    });
}

/// P4.3 section 4.10. With decisions switched off (this build) a Routine
/// Run's edit outside its grant behaves exactly as before and exactly as a
/// manual Run's: the ordinary lease path, its existing refusal, and no
/// decision command. Only an opened gate asks, once, for the Routine too.
#[test]
fn a_routine_runs_denied_edit_is_unchanged_while_decisions_are_off_and_asks_when_on() {
    assert!(!super::super::super::decision::EDIT_DECISIONS_AVAILABLE);
    let ordinary = vec![
        "reconcile",
        "native_lookup",
        "sync",
        "effect_fence",
        "ordinary_prepare",
    ];
    let mut logs = Vec::new();
    for routine in [false, true] {
        let mut f = Fixture::new();
        let arguments = denied_edit(&f);
        if routine {
            as_routine(&mut f);
        }
        let owner = DecisionOwner::new(&f);
        owner.script().ordinary_refusal = true;
        assert_eq!(
            edit_with(
                &f,
                &f.policy,
                &owner,
                arguments.clone(),
                EditDecisions::production()
            )
            .unwrap_err()
            .code,
            "native_owner_unavailable",
            "routine={routine}"
        );
        let s = owner.script();
        assert_eq!(s.created, 0, "no question, routine={routine}");
        assert!(s.proposals.is_empty());
        logs.push(s.log.clone());
        drop(s);
        assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);
    }
    assert_eq!(logs[0], ordinary);
    // The same path; a Routine Run additionally synchronizes its own grant
    // there, as it did before this change. No boot probe, no decision.
    assert_eq!(
        logs[1]
            .iter()
            .filter(|step| **step == "routine_sync")
            .count(),
        1
    );
    logs[1].retain(|step| *step != "routine_sync");
    assert_eq!(logs[1], logs[0], "a Routine Run takes the same path");

    // Gate opened: the Routine Run's denial becomes one decision.
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    as_routine(&mut f);
    let owner = DecisionOwner::new(&f);
    let answer = edit(&f, &owner, arguments.clone()).unwrap_err();
    assert_eq!(answer.code, "permission_pending");
    let s = owner.script();
    assert_eq!(s.created, 1, "one question");
    assert!(s.log.contains(&"prepare_decision"));
    assert!(!s.log.contains(&"ordinary_prepare"));
    drop(s);
    assert_eq!(text_count(&f, &arguments, "Needs approval"), 0);

    // A page the Routine's own grant selects is not a denial even when open.
    let mut f = Fixture::new();
    let arguments = denied_edit(&f);
    as_routine(&mut f);
    let document: DocumentId = serde_json::from_value(arguments["document_id"].clone()).unwrap();
    f.policy
        .evidence
        .proof
        .routine
        .as_mut()
        .unwrap()
        .policy
        .selected_pages
        .insert(document);
    let owner = DecisionOwner::new(&f);
    owner.script().ordinary_refusal = true;
    assert!(edit(&f, &owner, arguments).is_err());
    let s = owner.script();
    assert!(!s.log.contains(&"hello") && !s.log.contains(&"prepare_decision"));
    assert!(s.log.contains(&"ordinary_prepare"));
    assert_eq!(s.created, 0);
}
