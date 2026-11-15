// SPDX-License-Identifier: MPL-2.0
//! Koru's own save of an approved edit (O133 design B), driven through the
//! real shared effect ([`crate::native_effect`]) with the in-memory Store as
//! the executor.
//!
//! **Scripted delegate double.** `Ledger` answers the boot probe, the one-use
//! consume and the receipt the way the runtime and ledger answer them
//! (`koru_delegate_sqlite` `decisions::write::consume::consume_on`:
//! authentication and the immutable comparison precede replay; only the first
//! fresh consume carries the one Execute lease; a replay carries none). It is
//! not the real actor, IPC16 transport, ledger, provider, AI writer or a
//! Store worker on disk. No disk, network, browser or telemetry.
use super::*;
use koru_delegate_api::bots::{BotBootScan, BotCommand, BotReply};
use koru_delegate_core as d;
use koru_delegate_core::decisions::*;
use koru_delegate_core::{
    BotAccess, BotPolicyBarrier, BotPolicyBarrierKey, CapturedBotPolicy, NativeEffectPurpose,
    NativeResultDisposition, NativeResultIntentView, NativeResultObservation,
    NativeResultOperation, NativeResultStage, PrepareNativeResult,
};
use koru_mcp_server::native::NativeStoreIdentity;
use koru_mcp_server::{RequestContext, WorkspaceToolExecutor};
use koru_store_core::model::Scope;
use koru_store_core::{Locale, Timestamp};
use koru_store_server::memory::MemoryStore;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

const NOW_MS: i64 = 1_700_000_000_000;
const TEXT: &str = "Approved text";

fn r(value: u64) -> DecisionRevision {
    DecisionRevision::new(value).unwrap()
}
fn refusal(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation was refused.", false)
}

/// The current human proof the trigger reads off the writer.
struct Human {
    owner: koru_ai_core::bots::BotOwner,
    workspace: d::WorkspaceId,
    profile: Mutex<d::ProfileId>,
    unavailable: AtomicBool,
}
impl BotAuthority for Human {
    fn owner(&self) -> Result<koru_ai_core::bots::BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, workspace: d::WorkspaceId) -> Result<d::ProfileId, BotError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(BotError::Unavailable);
        }
        if workspace != self.workspace {
            return Err(BotError::Forbidden);
        }
        Ok(*self.profile.lock().unwrap())
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        if page.workspace == self.workspace {
            Ok(())
        } else {
            Err(BotError::Forbidden)
        }
    }
}

struct Script {
    owner: d::BotOwner,
    boot: d::DelegateBootId,
    binding: DecisionEditBinding,
    consumer: BotExecutionFence,
    authorization: DecisionAuthorization,
    intent: Option<NativeResultIntentView>,
    consumes: usize,
    leases: usize,
    receipts: usize,
    lose_consume_reply: bool,
    lose_receipt_reply: bool,
    no_boot: bool,
    log: Vec<&'static str>,
}
impl Script {
    fn consume(
        &mut self,
        fence: BotExecutionFence,
        request: PrepareNativeResult,
        payload: DecisionPayload,
        boot: d::DelegateBootId,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        self.consumes += 1;
        if boot != self.boot {
            return Err(refusal("bot_boot_stale"));
        }
        let b = &self.binding;
        // Authentication and the immutable comparison precede replay.
        if fence.owner != b.owner
            || fence.bot != b.bot
            || fence.run != b.run
            || payload != b.payload
            || request.request_fingerprint != b.edit_fingerprint
            || request.expected_revision.as_deref() != Some(b.expected_revision.as_slice())
            || request.target != Some(b.target)
            || request.key.operation_id != b.operation_id
            || request.config_revision != b.global_policy.config_revision.get()
            || request.policy_revision != b.global_policy.policy_revision.get()
        {
            return Err(refusal("bot_conflict"));
        }
        if let Some(intent) = &self.intent {
            // A replay licenses nothing: the retained intent, no Execute.
            let mut intent = intent.clone();
            intent.disposition = NativeResultDisposition::ReconcileOnly;
            return Ok(BotReply::NativeResult { intent });
        }
        if self.authorization != DecisionAuthorization::Granted
            || fence != self.consumer
            || request.key.turn != fence.turn.unwrap()
            || request.key.attempt != fence.attempt
            || request.key.generation != fence.generation
        {
            return Err(refusal("bot_fenced"));
        }
        self.leases += 1;
        self.authorization = DecisionAuthorization::Consumed;
        let intent = NativeResultIntentView {
            purpose: NativeEffectPurpose::Result {},
            key: request.key,
            kind: NativeResultOperation::EditPage,
            destination: request.destination,
            request_fingerprint: request.request_fingerprint,
            target: b.target,
            store_request: Uuid::new_v4(),
            stage: NativeResultStage::Prepared,
            verified: None,
            disposition: NativeResultDisposition::ReconcileOnly,
        };
        self.intent = Some(intent.clone());
        if std::mem::take(&mut self.lose_consume_reply) {
            // Committed, then the reply was lost.
            return Err(koru_delegate_api::Failure::unavailable());
        }
        Ok(BotReply::NativeResult {
            intent: NativeResultIntentView {
                disposition: NativeResultDisposition::Execute,
                ..intent
            },
        })
    }
}

struct Ledger(Mutex<Script>);
impl Ledger {
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.0.lock().unwrap()
    }
}
impl BotDelegatePort for Ledger {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut guard = self.script();
        let s = &mut *guard;
        match command {
            BotCommand::Hello { owner, scan } => {
                s.log.push("hello");
                assert_eq!(owner, s.owner);
                assert!(matches!(scan, BotBootScan::Probe {}));
                if s.no_boot {
                    return Err(koru_delegate_api::Failure::unavailable());
                }
                Ok(BotReply::Hello {
                    hello: d::BotHello {
                        boot: s.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
            BotCommand::ConsumeDecisionEdit {
                fence,
                request,
                payload,
                boot,
                ..
            } => {
                s.log.push("consume");
                s.consume(fence, request, payload, boot)
            }
            BotCommand::ConfirmNativeResult { request } => {
                s.log.push("receipt");
                let intent = s.intent.as_mut().expect("a leased intent");
                assert_eq!(intent.key, request.key);
                assert_eq!(intent.request_fingerprint, request.request_fingerprint);
                match request.observation {
                    NativeResultObservation::Durable { evidence } => {
                        s.receipts += 1;
                        intent.verified = Some(evidence);
                        intent.stage = NativeResultStage::Verified;
                        if std::mem::take(&mut s.lose_receipt_reply) {
                            return Err(koru_delegate_api::Failure::unavailable());
                        }
                        intent.stage = NativeResultStage::Registered;
                        intent.disposition = NativeResultDisposition::AlreadyRegistered;
                    }
                    NativeResultObservation::NotApplied { .. } => {
                        intent.stage = NativeResultStage::Refused;
                    }
                    _ => intent.stage = NativeResultStage::Unconfirmed,
                }
                Ok(BotReply::NativeResult {
                    intent: intent.clone(),
                })
            }
            _ => panic!("the trigger sends only the boot probe, the consume and the receipt"),
        }
    }
}

struct Fixture {
    memory: MemoryStore,
    workspace: d::WorkspaceId,
    profile: d::ProfileId,
    owner: d::BotOwner,
    document: d::DocumentId,
    arguments: Value,
    consumer: BotExecutionFence,
    view: DecisionReceiptView,
    proof: FreshBotPolicyProof,
}
impl Fixture {
    /// A page in the Bot's home, a person's AllowOnce of one edit to it, and
    /// the successor whose continuation was acknowledged.
    fn new() -> Self {
        let memory = MemoryStore::open("decision-apply-test").unwrap();
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
        let owner = d::BotOwner {
            user_scope: scope,
            device: d::DeviceId::new(),
        };
        let authority = NativeStoreAuthority {
            owner_scope: scope,
            expected_profile: profile,
            workspace,
        };
        let backend = native_store_backend(memory.client(), authority);
        let created = backend
            .prepare_native_write(
                "create_document",
                json!({"workspace_id":workspace,"title":"Native result"}),
            )
            .unwrap();
        let document = backend
            .execute_native_write(
                &created,
                NativeStoreIdentity {
                    candidate: d::DocumentId::new(),
                    scope: format!("koru.bot.native.v1:{scope}:fixture"),
                    request_key: Uuid::new_v4().to_string(),
                },
            )
            .unwrap()
            .document;
        let read = WorkspaceToolExecutor::new(backend.clone())
            .invoke(
                &RequestContext::selected(
                    "local",
                    "koru-bot-native",
                    [Scope::ReadDocuments],
                    [workspace],
                ),
                "read_document",
                json!({"workspace_id":workspace,"document_id":document}),
            )
            .unwrap();
        // The approved arguments, as the binding retains them (no operation_id).
        let arguments = json!({"workspace_id":workspace,"document_id":document,
            "base_revision":read["revision"],
            "operations":[{"op":"insert","parent_id":read["root_node_id"],"after_node_id":null,"kind":"paragraph","text":TEXT}]});
        let approved = backend
            .prepare_native_write("edit_document", arguments.clone())
            .unwrap();
        let description = approved.description();
        let bot = d::BotId::mint();
        let run = d::RunId::mint();
        let captured = CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
        };
        let proof = FreshBotPolicyProof {
            routine: None,
            global: BotPolicyBarrier {
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
        };
        let origin = BotExecutionFence {
            owner,
            bot,
            run,
            attempt: d::RunAttemptId::mint(),
            generation: d::AttemptGeneration::FIRST,
            session: d::SessionId::mint(),
            turn: Some(d::AgentTurnId::mint()),
        };
        let consumer = BotExecutionFence {
            attempt: d::RunAttemptId::mint(),
            generation: origin.generation.next(),
            session: d::SessionId::mint(),
            turn: Some(d::AgentTurnId::mint()),
            ..origin.clone()
        };
        let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
            version: DecisionVersionV1,
            owner,
            bot,
            run,
            origin_attempt: origin.attempt,
            origin_generation: origin.generation,
            origin_session: origin.session,
            origin_turn: origin.turn.unwrap(),
            submission: d::BotRequestId::mint(),
            routine: None,
            human_profile: profile,
            workspace,
            target: document,
            action: DecisionEditAction::EditDocument,
            operation_id: Uuid::new_v4(),
            edit_fingerprint: parse_fingerprint(&description.fingerprint).unwrap(),
            expected_revision: DecisionVersionVector::new(
                description.expected_revision.clone().unwrap(),
            )
            .unwrap(),
            payload_version: DecisionVersionV1,
            payload: DecisionPayload::new(serde_json::to_vec(&arguments).unwrap()).unwrap(),
            captured_policy: captured,
            global_policy: DecisionGlobalPolicy {
                key: proof.global.key.clone(),
                config_revision: r(1),
                policy_revision: r(1),
                digest: DecisionDigest::from_bytes([7; 32]),
            },
            routine_policy: None,
        });
        let binding_digest = binding.digest().unwrap();
        let decision = DecisionId::mint();
        let view = DecisionReceiptView {
            receipt: DecisionReceipt {
                decision,
                binding,
                binding_digest,
                deadline: DecisionDeadline {
                    created_boot: d::DelegateBootId::mint(),
                    created_at_ms: NOW_MS,
                    deadline_at_ms: NOW_MS + DECISION_WAIT_MILLIS,
                    native_deadline_at_ms: None,
                },
                state: DecisionState {
                    kind: DecisionKind::KoruEdit,
                    authorization: DecisionAuthorization::Granted,
                    phase: DecisionPhase::AwaitRetry,
                    revision: r(6),
                },
                answer: Some(DecisionAnswerReceipt {
                    decision,
                    request: DecisionAnswerId::mint(),
                    expected_revision: r(3),
                    committed_revision: r(4),
                    binding_digest,
                    human_profile: profile,
                    choice: DecisionAnswerChoice::AllowOnce {},
                    answered_at_ms: NOW_MS + 1,
                    authority_digest: DecisionDigest::from_bytes([9; 32]),
                }),
                consumed_at_ms: None,
                closure: None,
                last_observed_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
            },
            wait: BotWaitId::mint(),
            wait_phase: BotWaitPhase::ResumeReserved,
            wait_revision: r(5),
            native_response: None,
            resume: Some(DecisionResume {
                decision,
                origin: DecisionFence {
                    owner,
                    bot,
                    run,
                    attempt: origin.attempt,
                    generation: origin.generation,
                    session: origin.session,
                    turn: origin.turn.unwrap(),
                },
                consumer: DecisionConsumer::RecordedSuccessor {
                    attempt: consumer.attempt,
                    generation: consumer.generation,
                    creation_key: d::IdempotencyKey::mint(),
                    session: Some(consumer.session),
                },
                input_key: d::IdempotencyKey::mint(),
                input_fingerprint: [5; 32],
                state: DecisionResumeState::InputAcknowledged {
                    acknowledgment: DecisionInputAcknowledgment {
                        turn: consumer.turn.unwrap(),
                        receipt_digest: DecisionDigest::from_bytes([6; 32]),
                    },
                },
            }),
        };
        assert!(view.receipt.validate().is_ok(), "a valid granted receipt");
        Self {
            memory,
            workspace,
            profile,
            owner,
            document,
            arguments,
            consumer,
            view,
            proof,
        }
    }
    fn binding(&self) -> &DecisionEditBinding {
        match &self.view.receipt.binding {
            DecisionBindingV1::KoruEdit(binding) => binding,
            _ => unreachable!(),
        }
    }
    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(Ledger(Mutex::new(Script {
            owner: self.owner,
            boot: d::DelegateBootId::mint(),
            binding: self.binding().clone(),
            consumer: self.consumer.clone(),
            authorization: DecisionAuthorization::Granted,
            intent: None,
            consumes: 0,
            leases: 0,
            receipts: 0,
            lose_consume_reply: false,
            lose_receipt_reply: false,
            no_boot: false,
            log: Vec::new(),
        })))
    }
    fn human(&self) -> Arc<Human> {
        Arc::new(Human {
            owner: koru_ai_core::bots::BotOwner {
                user_scope: self.owner.user_scope,
                device: self.owner.device,
            },
            workspace: self.workspace,
            profile: Mutex::new(self.profile),
            unavailable: AtomicBool::new(false),
        })
    }
    fn work(&self, ledger: &Arc<Ledger>, human: &Arc<Human>) -> ApplyWork {
        ApplyWork {
            store: self.memory.client(),
            port: ledger.clone(),
            authority: human.clone(),
            view: self.view.clone(),
            consumer: self.consumer.clone(),
            proof: self.proof.clone(),
        }
    }
    fn authority(&self) -> NativeStoreAuthority {
        NativeStoreAuthority {
            owner_scope: self.owner.user_scope,
            expected_profile: self.profile,
            workspace: self.workspace,
        }
    }
    /// The model's exact retry of the approved edit, past the gateway's
    /// decision replay: the same shared consume the gateway's acknowledged
    /// consumer path runs (`run_mcp::native::dispatch::invoke`).
    fn model_retry(&self, ledger: &Ledger) -> Result<Value, &'static str> {
        let authority = self.authority();
        let backend = native_store_backend(self.memory.client(), authority.clone());
        let prepared = backend
            .prepare_native_write("edit_document", self.arguments.clone())
            .unwrap();
        let fingerprint = parse_fingerprint(&prepared.description().fingerprint).unwrap();
        let binding = self.binding();
        let key = NativeResultKey {
            run: self.consumer.run,
            attempt: self.consumer.attempt,
            generation: self.consumer.generation,
            turn: self.consumer.turn.unwrap(),
            operation_id: binding.operation_id,
        };
        let boot = ledger.script().boot;
        consume(
            &Notes::default(),
            &self.memory.client(),
            ledger,
            &backend,
            &authority,
            NativeEffectSubject {
                owner: self.owner,
                bot: binding.bot,
            },
            &prepared,
            &ApprovedEdit {
                fence: &self.consumer,
                key: &key,
                fingerprint,
                payload: &binding.payload,
            },
            &self.view,
            &self.proof,
            boot,
        )
    }
    /// How often the approved text appears in the page read. Whatever that
    /// count is after the one save, no later step may change it.
    fn text_count(&self) -> usize {
        let backend = native_store_backend(self.memory.client(), self.authority());
        WorkspaceToolExecutor::new(backend)
            .invoke(
                &RequestContext::selected(
                    "local",
                    "koru-bot-native",
                    [Scope::ReadDocuments],
                    [self.workspace],
                ),
                "read_document",
                json!({"workspace_id":self.workspace,"document_id":self.document}),
            )
            .unwrap()
            .to_string()
            .matches(TEXT)
            .count()
    }
    fn wait_key(&self) -> BotWaitEnrollmentKey {
        let binding = self.binding();
        BotWaitCleanupIntentV1 {
            version: DecisionVersionV1,
            owner: binding.owner,
            bot: binding.bot,
            run: binding.run,
            wait_id: self.view.wait,
            decision_id: self.view.receipt.decision,
            decision_revision: r(2),
            wait_revision: r(2),
            origin_attempt: binding.origin_attempt,
            origin_generation: binding.origin_generation,
            origin_session: binding.origin_session,
            origin_turn: binding.origin_turn,
            cause: BotWaitCleanupCause::DecisionWaitCleanup,
        }
        .key()
        .unwrap()
    }
}

fn state(authorization: DecisionAuthorization, phase: DecisionPhase) -> DecisionState {
    DecisionState {
        kind: DecisionKind::KoruEdit,
        authorization,
        phase,
        revision: r(6),
    }
}

#[test]
fn allow_saves_the_approved_edit_once_through_the_shared_executor_with_no_model_call() {
    let f = Fixture::new();
    let ledger = f.ledger();
    let human = f.human();
    assert_eq!(f.text_count(), 0);
    let report = f.work(&ledger, &human).run();
    assert_eq!(
        report,
        ApplyReport {
            outcome: ApplyOutcome::Saved,
            settled: true,
            uncertain: None,
        }
    );
    let s = ledger.script();
    // One consume, its one lease, one receipt; nothing a model would send
    // (no decision preparation, no ordinary lease) and no provider exists.
    assert_eq!((s.consumes, s.leases, s.receipts), (1, 1, 1));
    assert_eq!(s.log, vec!["hello", "consume", "receipt"]);
    let intent = s.intent.as_ref().unwrap();
    assert_eq!(intent.stage, NativeResultStage::Registered);
    assert_eq!(intent.key.attempt, f.consumer.attempt, "the successor's consume");
    assert_eq!(intent.key.turn, f.consumer.turn.unwrap());
    drop(s);
    assert!(f.text_count() > 0, "the approved edit was saved");
}

#[test]
fn a_lost_receipt_reply_reconciles_on_the_next_read_and_never_saves_twice() {
    let f = Fixture::new();
    let ledger = f.ledger();
    let human = f.human();
    ledger.script().lose_receipt_reply = true;
    let mut progress = ApplyProgress::new(f.consumer.clone());
    let granted = state(DecisionAuthorization::Granted, DecisionPhase::AwaitRetry);
    assert!(progress.due(&granted));
    progress.start();
    let first = f.work(&ledger, &human).run();
    assert_eq!(first.outcome, ApplyOutcome::Unknown);
    progress.finish(first.outcome);
    let once = f.text_count();
    assert!(once > 0, "the Store applied the edit before the reply was lost");
    // The next read sees the consume committed: only reconciliation is due.
    let consumed = state(DecisionAuthorization::Consumed, DecisionPhase::Effect);
    assert!(progress.due(&consumed));
    progress.start();
    let second = f.work(&ledger, &human).run();
    assert_eq!(second.outcome, ApplyOutcome::Saved);
    progress.finish(second.outcome);
    assert!(!progress.due(&consumed), "done after the saved receipt");
    let s = ledger.script();
    assert_eq!((s.consumes, s.leases), (2, 1), "the replay licensed nothing");
    assert_eq!(s.intent.as_ref().unwrap().stage, NativeResultStage::Registered);
    drop(s);
    assert_eq!(f.text_count(), once, "never a second save");
}

#[test]
fn a_lost_consume_reply_is_never_run_blind() {
    let f = Fixture::new();
    let ledger = f.ledger();
    let human = f.human();
    ledger.script().lose_consume_reply = true;
    assert_eq!(
        f.work(&ledger, &human).run().outcome,
        ApplyOutcome::Unknown
    );
    // The replay carries no Execute and the Store holds nothing for the
    // original request: still unknown, and nothing runs.
    assert_eq!(
        f.work(&ledger, &human).run().outcome,
        ApplyOutcome::Unknown
    );
    let s = ledger.script();
    assert_eq!((s.consumes, s.leases, s.receipts), (2, 1, 0));
    assert!(!s.log.contains(&"receipt"));
    drop(s);
    assert_eq!(f.text_count(), 0);
}

#[test]
fn the_model_repeating_the_edit_gets_the_retained_result_and_no_second_save() {
    // Koru first, then the model repeats it anyway.
    let f = Fixture::new();
    let ledger = f.ledger();
    let human = f.human();
    assert_eq!(f.work(&ledger, &human).run().outcome, ApplyOutcome::Saved);
    let once = f.text_count();
    let repeated = f.model_retry(&ledger).unwrap();
    assert_eq!(repeated["status"], "saved");
    assert_eq!(
        repeated["operation_id"],
        json!(f.binding().operation_id),
        "the one approved change's result"
    );
    let s = ledger.script();
    assert_eq!((s.consumes, s.leases, s.receipts), (2, 1, 1));
    drop(s);
    assert_eq!(f.text_count(), once);

    // The model first (it repeated before Koru's attempt), then Koru: Koru's
    // consume replays the retained result.
    let f = Fixture::new();
    let ledger = f.ledger();
    let human = f.human();
    assert_eq!(f.model_retry(&ledger).unwrap()["status"], "saved");
    let once = f.text_count();
    assert_eq!(f.work(&ledger, &human).run().outcome, ApplyOutcome::Saved);
    let s = ledger.script();
    assert_eq!((s.consumes, s.leases, s.receipts), (2, 1, 1));
    drop(s);
    assert_eq!(f.text_count(), once);
}

#[test]
fn without_the_trigger_or_its_receipt_the_models_exact_retry_saves_once() {
    let f = Fixture::new();
    let key = f.wait_key();
    let origin = f.view.receipt.binding.origin();
    assert!(approved_receipt(Some(&f.view), &key, &origin).is_some());
    // The receipt is gone (an AI restart, O146): nothing to apply.
    assert!(approved_receipt(None, &key, &origin).is_none());
    // A receipt for another decision or origin is not this wait's.
    let mut other = f.view.clone();
    other.receipt.decision = DecisionId::mint();
    assert!(approved_receipt(Some(&other), &key, &origin).is_none());
    let mut moved = origin;
    moved.attempt = d::RunAttemptId::mint();
    assert!(approved_receipt(Some(&f.view), &key, &moved).is_none());

    // The trigger cannot reach the owner or read the human: nothing is sent.
    let ledger = f.ledger();
    let human = f.human();
    ledger.script().no_boot = true;
    assert_eq!(
        f.work(&ledger, &human).run().outcome,
        ApplyOutcome::Unavailable
    );
    ledger.script().no_boot = false;
    human.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        f.work(&ledger, &human).run().outcome,
        ApplyOutcome::Unavailable
    );
    assert_eq!(ledger.script().consumes, 0);
    assert_eq!(f.text_count(), 0);
    // The fallback: the model's exact retry saves the approved edit once.
    assert_eq!(f.model_retry(&ledger).unwrap()["status"], "saved");
    let s = ledger.script();
    assert_eq!((s.consumes, s.leases, s.receipts), (1, 1, 1));
    drop(s);
    assert!(f.text_count() > 0);
}

#[test]
fn deny_or_expiry_saves_nothing() {
    let f = Fixture::new();
    let progress = ApplyProgress::new(f.consumer.clone());
    for authorization in [
        DecisionAuthorization::Pending,
        DecisionAuthorization::Denied,
        DecisionAuthorization::Expired,
        DecisionAuthorization::Canceled,
    ] {
        for phase in [
            DecisionPhase::Held,
            DecisionPhase::AwaitRetry,
            DecisionPhase::Effect,
            DecisionPhase::Settled,
        ] {
            assert!(
                !progress.due(&state(authorization, phase)),
                "{authorization:?}/{phase:?}"
            );
        }
    }
    // Consumed by someone else, with nothing of Koru's own to reconcile.
    assert!(!progress.due(&state(
        DecisionAuthorization::Consumed,
        DecisionPhase::Effect
    )));
    // A native approval is never this trigger's.
    let mut native = state(DecisionAuthorization::Granted, DecisionPhase::AwaitRetry);
    native.kind = DecisionKind::NativeApproval;
    assert!(!progress.due(&native));
    // Even if an attempt ran, the owner refuses a decision that is not
    // granted, and nothing is saved.
    for authorization in [DecisionAuthorization::Denied, DecisionAuthorization::Expired] {
        let ledger = f.ledger();
        ledger.script().authorization = authorization;
        assert_eq!(
            f.work(&ledger, &f.human()).run().outcome,
            ApplyOutcome::Refused
        );
        assert_eq!(ledger.script().leases, 0);
    }
    assert_eq!(f.text_count(), 0);
}

#[test]
fn a_stale_human_or_policy_saves_nothing() {
    let f = Fixture::new();
    // The home's profile is no longer the decision's human.
    let ledger = f.ledger();
    let human = f.human();
    *human.profile.lock().unwrap() =
        serde_json::from_value(json!(Uuid::new_v4())).unwrap();
    assert_eq!(
        f.work(&ledger, &human).run().outcome,
        ApplyOutcome::Refused
    );
    assert!(ledger.script().log.is_empty(), "nothing was sent");
    // Another owner answers the authority read.
    let ledger = f.ledger();
    let mut stranger = Human {
        owner: f.human().owner,
        workspace: f.workspace,
        profile: Mutex::new(f.profile),
        unavailable: AtomicBool::new(false),
    };
    stranger.owner.device = d::DeviceId::new();
    assert_eq!(
        f.work(&ledger, &Arc::new(stranger)).run().outcome,
        ApplyOutcome::Refused
    );
    assert!(ledger.script().log.is_empty());
    // The Bot's policy moved past the binding's revisions, or its effects
    // are off (an Archive settling it).
    let stale: [fn(&mut FreshBotPolicyProof); 3] = [
        |p| p.global.config_revision = 2,
        |p| p.global.policy_revision = 2,
        |p| p.global.effects_allowed = false,
    ];
    for stale in stale {
        let ledger = f.ledger();
        let mut work = f.work(&ledger, &f.human());
        stale(&mut work.proof);
        assert_eq!(work.run().outcome, ApplyOutcome::Refused);
        assert!(ledger.script().log.is_empty());
    }
    assert_eq!(f.text_count(), 0);
}

#[test]
fn a_persistently_failing_trigger_makes_a_bounded_number_of_attempts() {
    let f = Fixture::new();
    let granted = state(DecisionAuthorization::Granted, DecisionPhase::AwaitRetry);
    let consumed = state(DecisionAuthorization::Consumed, DecisionPhase::Effect);
    for failure in [ApplyOutcome::Unavailable, ApplyOutcome::Unknown] {
        let mut progress = ApplyProgress::new(f.consumer.clone());
        // Many more consumer reads than attempts; each read may start one.
        for read in 0..(4 * MAX_APPLY_ATTEMPTS) {
            let observed = if failure == ApplyOutcome::Unknown && read > 0 {
                &consumed
            } else {
                &granted
            };
            if progress.due(observed) {
                progress.start();
                // One job at a time: a read while it runs starts nothing.
                assert!(!progress.due(observed));
                progress.finish(failure);
            }
        }
        assert_eq!(progress.attempts(), MAX_APPLY_ATTEMPTS, "{failure:?}");
        assert!(!progress.due(&granted));
    }
    // A definite refusal stops at once; the model's retry is the fallback.
    let mut progress = ApplyProgress::new(f.consumer.clone());
    progress.start();
    progress.finish(ApplyOutcome::Refused);
    assert!(!progress.due(&granted));
    assert_eq!(progress.attempts(), 1);
    // Nothing to apply in this process stops without an attempt.
    let mut progress = ApplyProgress::new(f.consumer.clone());
    progress.stop();
    assert!(!progress.due(&granted));
    assert_eq!(progress.attempts(), 0);
}
