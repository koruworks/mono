// SPDX-License-Identifier: MPL-2.0

//! The private-connection world for tests (M5 pieces F and G): the real
//! connection service over the scripted vendor, the lockable in-memory
//! credential store, the scripted effect ledger and the real ai.db writer on
//! an in-memory database, and a scripted delegate that answers the private
//! Bot commands the way the actor does.
//!
//! No socket, no browser, no OS keychain, no real vendor, no file. The
//! scripted delegate is not the real actor, ledger or IPC 18 transport.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use koru_ai_core::connections::{
    Connection, ConnectionError, ConnectionId, ConnectionItem, ConnectionReceiptStage,
};
use koru_ai_sqlite::Repository;
use koru_connections::support::{FakeClock, FakeLedger, FakeVendor, Journal, LockableCredentials};
use koru_connections::{
    ConnectionPorts, ConnectionService, EffectLedger, LedgerError, NoConnectionChangeSink,
    TripStart, process_dispatcher,
};
use koru_delegate_api::bots::{BotCommand, BotDelegatePort, BotReply, DecisionReceiptView};
use koru_delegate_core::decisions::*;
use koru_delegate_core::{
    BotAdmissionState, BotAdmissionView, BotExecutionFence, BotHello, BotPolicyBarrierReceipt,
    BotPolicyBarrierState, BotRequestId, CapturedBotPolicy, DelegateBootId, FreshBotPolicyProof,
    PrivateEffectKey, PrivateOperationKind, PrivateOperationProposal, ProfileId,
    private_request_fingerprint,
};
use koru_oauth_credentials::CredentialContext;
use uuid::Uuid;

use crate::worker::connections::store::{RowStore, Rows};

pub const SUBJECT_A: &str = "subject-a";
pub const SUBJECT_B: &str = "subject-b";
pub const LABEL_A: &str = "MARK-LABEL-a@example.test";
pub const TITLE_1: &str = "MARK-TITLE-1";
pub const CONTENT_1: &str = "MARK-CONTENT of the first file";
const REDIRECT: &str = "http://127.0.0.1:49152/callback";
const NOW_MS: i64 = 1_700_000_000_000;

/// The real ai.db writer, one transaction per call, behind the same mapping
/// production uses.
pub struct LockedRows(Mutex<Repository>);
impl Rows for LockedRows {
    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Repository) -> Result<T, ConnectionError> + Send + 'static,
    ) -> Result<T, ConnectionError> {
        work(
            &mut self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

pub struct PrivateWorld {
    pub clock: Arc<FakeClock>,
    pub journal: Journal,
    pub vendor: Arc<FakeVendor>,
    pub credentials: Arc<LockableCredentials>,
    pub ledger: Arc<FakeLedger>,
    pub service: Arc<ConnectionService>,
    pub owner: koru_ai_core::bots::BotOwner,
    pub context: CredentialContext,
    store: Arc<RowStore<LockedRows>>,
}

impl PrivateWorld {
    /// Two accounts at the scripted vendor, three files, nothing connected.
    pub fn new(owner: koru_delegate_core::BotOwner) -> Self {
        let clock = Arc::new(FakeClock::default());
        let journal = Journal::default();
        let vendor = Arc::new(FakeVendor::new(clock.clone(), journal.clone()));
        vendor.add_account(SUBJECT_A, LABEL_A);
        vendor.add_account(SUBJECT_B, "MARK-LABEL-b@example.test");
        vendor.add_item("doc-1", SUBJECT_A, TITLE_1, CONTENT_1);
        vendor.add_item("doc-2", SUBJECT_A, "MARK-TITLE-2", "MARK-CONTENT two");
        vendor.add_item("doc-b", SUBJECT_B, "MARK-TITLE-b", "MARK-CONTENT b");
        let owner = koru_ai_core::bots::BotOwner {
            user_scope: owner.user_scope,
            device: owner.device,
        };
        // The root is a name, never opened: the context reads no file.
        let context = CredentialContext::new("nightly", Path::new("/isolated/root"), owner.device)
            .expect("a credential context");
        let credentials = Arc::new(LockableCredentials::default());
        let ledger = Arc::new(FakeLedger::new(journal.clone()));
        let store = Arc::new(RowStore(LockedRows(Mutex::new(
            Repository::in_memory().expect("an in-memory ai.db"),
        ))));
        let service = Arc::new(ConnectionService::new(ConnectionPorts {
            vendor: vendor.clone(),
            credentials: credentials.clone(),
            context: context.clone(),
            store: store.clone(),
            ledger: ledger.clone(),
            clock: clock.clone(),
            changes: Arc::new(NoConnectionChangeSink),
        }));
        Self {
            clock,
            journal,
            vendor,
            credentials,
            ledger,
            service,
            owner,
            context,
            store,
        }
    }

    /// One read of ai.db's rows.
    pub fn rows<T>(&self, read: impl FnOnce(&mut Repository) -> T) -> T {
        read(
            &mut self
                .store
                .0
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Connect `subject` under `name`, picking `picked`.
    pub fn connect(&self, name: &str, subject: &str, picked: &[&str]) -> Connection {
        let request = koru_ai_core::bots::BotRequestId::mint();
        let consent = match self
            .service
            .begin_connect(self.owner, request, name, REDIRECT)
            .expect("the connect begins")
        {
            TripStart::Consent { consent, .. } => consent,
            TripStart::Finished(receipt) => panic!("the connect did not begin: {receipt:?}"),
        };
        let receipt = self
            .service
            .complete_trip(
                self.owner,
                request,
                self.vendor.approve(&consent, subject, picked),
            )
            .expect("the connect completes");
        assert_eq!(receipt.stage, ConnectionReceiptStage::Completed);
        self.connection(receipt.connection.expect("a connection"))
    }
    pub fn connection(&self, id: ConnectionId) -> Connection {
        self.rows(|r| r.connection_by_id(self.owner, id))
            .expect("ai.db answers")
            .expect("the connection")
    }
    pub fn item(&self, id: ConnectionId, reference: &str) -> ConnectionItem {
        self.rows(|r| r.connection_items(self.owner, id))
            .expect("ai.db answers")
            .into_iter()
            .find(|item| item.item_ref == reference)
            .unwrap_or_else(|| panic!("no picked item {reference}"))
    }
    /// The operation the AI owner plans for this item now.
    pub fn proposal(
        &self,
        operation_id: Uuid,
        id: ConnectionId,
        reference: &str,
    ) -> PrivateOperationProposal {
        let connection = self.connection(id);
        let item = self.item(id, reference);
        let item_ref = DecisionDigest::from_bytes(item.item_ref_digest);
        PrivateOperationProposal {
            operation_id,
            connection: id,
            connection_revision: DecisionRevision::new(connection.revision).expect("a revision"),
            account: DecisionDigest::from_bytes(connection.account_subject_digest),
            item: item.id,
            item_ref,
            operation: PrivateOperationKind::ReadItem,
            request_fingerprint: private_request_fingerprint(
                PrivateOperationKind::ReadItem,
                id,
                item.id,
                item_ref,
            ),
        }
    }
}

impl PrivateWorld {
    /// What a worker is composed with to host this world's vendor and
    /// credential store. The worker brings its own ai.db.
    pub fn setup(&self) -> crate::ConnectionsSetup {
        crate::ConnectionsSetup {
            vendor: self.vendor.clone(),
            credentials: self.credentials.clone(),
            context: self.context.clone(),
        }
    }
}

/// A Run's owner on this computer.
pub fn owner() -> koru_delegate_core::BotOwner {
    koru_delegate_core::BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: koru_delegate_core::DeviceId::new(),
    }
}

/// A live consumer of a new Run of a new Bot.
pub fn fence(owner: koru_delegate_core::BotOwner) -> BotExecutionFence {
    BotExecutionFence {
        owner,
        bot: koru_delegate_core::BotId::mint(),
        run: koru_delegate_core::RunId::mint(),
        attempt: koru_delegate_core::RunAttemptId::mint(),
        generation: koru_delegate_core::AttemptGeneration::FIRST,
        session: koru_delegate_core::SessionId::mint(),
        turn: Some(koru_delegate_core::AgentTurnId::mint()),
    }
}

/// The same Run's recorded successor: another attempt, session and turn.
pub fn successor(origin: &BotExecutionFence) -> BotExecutionFence {
    BotExecutionFence {
        attempt: koru_delegate_core::RunAttemptId::mint(),
        session: koru_delegate_core::SessionId::mint(),
        turn: Some(koru_delegate_core::AgentTurnId::mint()),
        ..origin.clone()
    }
}

/// A Bot's policy homed in `workspace`, holding read grants for `granted`.
pub fn captured(
    workspace: koru_delegate_core::WorkspaceId,
    granted: &[ConnectionId],
) -> CapturedBotPolicy {
    CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: koru_delegate_core::BotAccess::Default,
        selected_pages: Default::default(),
        connections: granted
            .iter()
            .map(|connection| koru_delegate_core::ConnectionGrantRef {
                connection: *connection,
                operation: PrivateOperationKind::ReadItem,
            })
            .collect(),
    }
}

/// The Bot's current barrier holding `current`, with effects allowed.
pub fn proof(fence: &BotExecutionFence, current: &CapturedBotPolicy) -> FreshBotPolicyProof {
    FreshBotPolicyProof {
        routine: None,
        global: koru_delegate_core::BotPolicyBarrier {
            key: koru_delegate_core::BotPolicyBarrierKey {
                owner: fence.owner,
                bot: fence.bot,
                request: Uuid::new_v4().try_into().expect("a request id"),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: current.clone(),
            effects_allowed: true,
        },
    }
}

fn revision(value: u64) -> DecisionRevision {
    DecisionRevision::new(value).expect("a revision")
}
fn refusal(code: &str) -> koru_delegate_api::Failure {
    koru_delegate_api::Failure::new(code, "The Bot operation was refused.", false)
}

/// What the scripted delegate holds and how the next commands are answered.
pub struct Script {
    pub fence: BotExecutionFence,
    pub captured: CapturedBotPolicy,
    pub boot: DelegateBootId,
    pub human: ProfileId,
    pub submission: BotRequestId,
    /// The delegate's own grant predicate for the granted path.
    pub granted: bool,
    /// Every command received, by name, in order.
    pub log: Vec<&'static str>,
    /// Refuse the next command of this name with this code. Nothing commits.
    pub refuse: HashMap<&'static str, &'static str>,
    /// The next command of this name commits and its reply is lost.
    pub lose: HashSet<&'static str>,
    /// The Run's one decision, once asked.
    pub view: Option<DecisionReceiptView>,
    /// Replays answered before the initial projection is acknowledged.
    pub acknowledge_after: usize,
    pub decisions_created: usize,
    replays: usize,
    consumer: Option<BotExecutionFence>,
    effects: HashMap<Uuid, PrivateEffectKey>,
    pub announces: usize,
}

/// A delegate that answers the private Bot commands (IPC 18) and the few
/// existing ones a private read sends, over the scripted effect ledger.
pub struct PrivateDelegate {
    script: Mutex<Script>,
    ledger: Arc<FakeLedger>,
}

impl PrivateDelegate {
    pub fn new(
        world: &PrivateWorld,
        fence: BotExecutionFence,
        captured: CapturedBotPolicy,
    ) -> Self {
        Self {
            ledger: world.ledger.clone(),
            script: Mutex::new(Script {
                fence,
                captured,
                boot: DelegateBootId::mint(),
                human: serde_json::from_value(serde_json::json!(Uuid::new_v4()))
                    .expect("a profile id"),
                submission: BotRequestId::mint(),
                granted: true,
                log: Vec::new(),
                refuse: HashMap::new(),
                lose: HashSet::new(),
                view: None,
                acknowledge_after: 1,
                decisions_created: 0,
                replays: 0,
                consumer: None,
                effects: HashMap::new(),
                announces: 0,
            }),
        }
    }
    pub fn script(&self) -> MutexGuard<'_, Script> {
        self.script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    pub fn log(&self) -> Vec<&'static str> {
        self.script().log.clone()
    }
    pub fn count(&self, name: &str) -> usize {
        self.script()
            .log
            .iter()
            .filter(|entry| **entry == name)
            .count()
    }
    /// The retained effect of an operation, as any later reply reports it.
    pub fn effect(&self, operation_id: Uuid) -> Option<koru_delegate_core::PrivateEffectView> {
        let key = self.script().effects.get(&operation_id).cloned()?;
        Some(self.ledger.row(&key))
    }

    /// The person allowed it and the recorded successor's input was
    /// acknowledged: `granted/await_retry` with the acknowledged resume. The
    /// Run's live fence is the successor's from here.
    pub fn allow(&self, successor: BotExecutionFence) {
        let mut script = self.script();
        let human = script.human;
        let view = script.view.as_mut().expect("a prepared decision");
        let receipt = &mut view.receipt;
        receipt.answer = Some(DecisionAnswerReceipt {
            decision: receipt.decision,
            request: DecisionAnswerId::mint(),
            expected_revision: revision(3),
            committed_revision: revision(4),
            binding_digest: receipt.binding_digest,
            human_profile: human,
            choice: DecisionAnswerChoice::AllowOnce {},
            answered_at_ms: NOW_MS + 1,
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        });
        receipt.state = DecisionState {
            kind: DecisionKind::PrivateOperation,
            authorization: DecisionAuthorization::Granted,
            phase: DecisionPhase::AwaitRetry,
            revision: revision(6),
        };
        view.wait_phase = BotWaitPhase::ResumeReserved;
        view.resume = Some(DecisionResume {
            decision: receipt.decision,
            origin: receipt.binding.origin(),
            consumer: DecisionConsumer::RecordedSuccessor {
                attempt: successor.attempt,
                generation: successor.generation,
                creation_key: Uuid::new_v4().to_string().parse().expect("a key"),
                session: Some(successor.session),
            },
            input_key: Uuid::new_v4().to_string().parse().expect("a key"),
            input_fingerprint: [5; 32],
            state: DecisionResumeState::InputAcknowledged {
                acknowledgment: DecisionInputAcknowledgment {
                    turn: successor.turn.expect("a turn"),
                    receipt_digest: DecisionDigest::from_bytes([6; 32]),
                },
            },
        });
        script.consumer = Some(successor.clone());
        script.fence = successor;
    }

    /// The person did not allow it.
    pub fn deny(&self) {
        let mut script = self.script();
        let human = script.human;
        let receipt = &mut script.view.as_mut().expect("a prepared decision").receipt;
        receipt.answer = Some(DecisionAnswerReceipt {
            decision: receipt.decision,
            request: DecisionAnswerId::mint(),
            expected_revision: revision(3),
            committed_revision: revision(4),
            binding_digest: receipt.binding_digest,
            human_profile: human,
            choice: DecisionAnswerChoice::Deny {},
            answered_at_ms: NOW_MS + 1,
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        });
        receipt.state = DecisionState {
            kind: DecisionKind::PrivateOperation,
            authorization: DecisionAuthorization::Denied,
            phase: DecisionPhase::StopPending,
            revision: revision(5),
        };
    }
}

impl Script {
    fn view_for(
        &self,
        fence: &BotExecutionFence,
        operation: &PrivateOperationProposal,
        proof: &FreshBotPolicyProof,
    ) -> DecisionReceiptView {
        let binding = DecisionBindingV1::PrivateOperation(DecisionPrivateBinding {
            version: DecisionVersionV1,
            owner: fence.owner,
            bot: fence.bot,
            run: fence.run,
            origin_attempt: fence.attempt,
            origin_generation: fence.generation,
            origin_session: fence.session,
            origin_turn: fence.turn.expect("a turn"),
            submission: self.submission,
            routine: None,
            human_profile: self.human,
            workspace: self.captured.workspace,
            connection: operation.connection,
            connection_revision: operation.connection_revision,
            account: operation.account,
            item: operation.item,
            item_ref: operation.item_ref,
            operation: operation.operation,
            operation_id: operation.operation_id,
            request_fingerprint: operation.request_fingerprint,
            captured_policy: self.captured.clone(),
            global_policy: DecisionGlobalPolicy {
                key: proof.global.key.clone(),
                config_revision: revision(proof.global.config_revision),
                policy_revision: revision(proof.global.policy_revision),
                digest: DecisionDigest::from_bytes([7; 32]),
            },
            routine_policy: None,
        });
        let binding_digest = binding.digest().expect("a valid private binding");
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
                    kind: DecisionKind::PrivateOperation,
                    authorization: DecisionAuthorization::Pending,
                    phase: DecisionPhase::Publishing,
                    revision: revision(1),
                },
                answer: None,
                consumed_at_ms: None,
                closure: None,
                last_observed_at_ms: NOW_MS,
                updated_at_ms: NOW_MS,
            },
            wait: BotWaitId::mint(),
            wait_phase: BotWaitPhase::Fenced,
            wait_revision: revision(1),
            native_response: None,
            resume: None,
        }
    }

    fn key(&self, fence: &BotExecutionFence, operation_id: Uuid) -> PrivateEffectKey {
        PrivateEffectKey {
            run: fence.run,
            operation_id,
            attempt: fence.attempt,
            generation: fence.generation,
            turn: fence.turn.expect("a turn"),
        }
    }

    /// A scripted refusal or lost reply for the command named `name`.
    fn scripted(&mut self, name: &'static str) -> Result<bool, koru_delegate_api::Failure> {
        self.log.push(name);
        if let Some(code) = self.refuse.remove(name) {
            return Err(refusal(code));
        }
        Ok(self.lose.remove(name))
    }
}

impl BotDelegatePort for PrivateDelegate {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let lost = || Err(koru_delegate_api::Failure::unavailable());
        // The ledger is locked on its own: an advance never holds the script.
        if let BotCommand::AdvancePrivateEffect {
            key,
            dispatcher,
            transition,
        } = &command
        {
            self.script().log.push("advance");
            return match self.ledger.advance(key, *dispatcher, transition) {
                Ok(effect) => Ok(BotReply::PrivateEffect { effect }),
                Err(LedgerError::Stale) => Err(refusal("bot_private_effect_stale")),
                Err(LedgerError::Refused) => Err(refusal("bot_invalid")),
                Err(LedgerError::Unavailable) => lost(),
            };
        }
        let mut s = self.script();
        match command {
            BotCommand::Hello { owner, .. } => {
                s.log.push("hello");
                assert_eq!(owner, s.fence.owner);
                Ok(BotReply::Hello {
                    hello: BotHello {
                        boot: s.boot,
                        entries: vec![],
                        next: None,
                    },
                })
            }
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
            BotCommand::SynchronizePolicy { barrier } => {
                s.log.push("sync");
                Ok(BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: BotPolicyBarrierState::Effective,
                    },
                })
            }
            BotCommand::SynchronizeRoutinePolicy { barrier } => {
                s.log.push("routine_sync");
                Ok(BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: BotPolicyBarrierState::Effective,
                })
            }
            BotCommand::EffectFence { fence } => {
                s.log.push("effect_fence");
                Ok(BotReply::EffectFence {
                    fence,
                    policy: s.captured.clone(),
                })
            }
            BotCommand::AnnouncePrivateDispatcher { owner, dispatcher } => {
                if s.scripted("announce")? {
                    return lost();
                }
                assert_eq!(owner, s.fence.owner);
                s.announces += 1;
                Ok(BotReply::PrivateDispatcher {
                    dispatcher,
                    resolved: 0,
                })
            }
            BotCommand::ReadPrivateEffect {
                run, operation_id, ..
            } => {
                if s.scripted("read_effect")? {
                    return lost();
                }
                match s.effects.get(&operation_id) {
                    Some(key) if key.run == run => Ok(BotReply::PrivateEffect {
                        effect: self.ledger.row(key),
                    }),
                    _ => Ok(BotReply::Missing),
                }
            }
            BotCommand::PreparePrivateEffect {
                fence,
                dispatcher,
                operation,
                boot,
                ..
            } => {
                let lose = s.scripted("prepare_effect")?;
                assert_eq!(boot, s.boot, "the probed delegate boot");
                assert_eq!(dispatcher, process_dispatcher());
                // While a decision's wait is open it owns the Run (O147).
                if s.view
                    .as_ref()
                    .is_some_and(|view| view.receipt.state.phase != DecisionPhase::Settled)
                {
                    return Err(refusal("bot_decision_owned"));
                }
                if !s.granted {
                    return Err(refusal("bot_connection_not_granted"));
                }
                if let Some(key) = s.effects.get(&operation.operation_id) {
                    let effect = self.ledger.row(key);
                    return if effect.operation == operation {
                        Ok(BotReply::PrivateEffect { effect })
                    } else {
                        Err(refusal("bot_conflict"))
                    };
                }
                let key = s.key(&fence, operation.operation_id);
                s.effects.insert(operation.operation_id, key.clone());
                let effect = self.ledger.open_keyed(key, operation, dispatcher, None);
                if lose {
                    return lost();
                }
                Ok(BotReply::PrivateEffect { effect })
            }
            BotCommand::PreparePrivateDecision {
                fence,
                operation,
                proof,
                boot,
            } => {
                let lose = s.scripted("prepare_decision")?;
                assert_eq!(boot, s.boot, "the probed delegate boot");
                if s.view.is_none() {
                    if s.granted && s.captured.grants_connection(&operation.grant()) {
                        return Err(refusal("bot_connection_granted"));
                    }
                    let view = s.view_for(&fence, &operation, &proof);
                    s.view = Some(view.clone());
                    s.decisions_created += 1;
                    if lose {
                        return lost();
                    }
                    return Ok(BotReply::Decision { decision: view });
                }
                let acknowledge_after = s.acknowledge_after;
                s.replays += 1;
                let replays = s.replays;
                let view = s.view.as_mut().expect("a retained decision");
                if view.receipt.binding.operation_id() != Some(operation.operation_id) {
                    return Err(refusal("bot_decision_owned"));
                }
                if !view.receipt.binding.matches_private(&operation) {
                    return Err(refusal("bot_conflict"));
                }
                let state = &mut view.receipt.state;
                if state.authorization == DecisionAuthorization::Pending
                    && state.phase == DecisionPhase::Publishing
                    && replays >= acknowledge_after
                {
                    state.phase = DecisionPhase::Holding;
                    state.revision = revision(state.revision.get() + 1);
                }
                Ok(BotReply::Decision {
                    decision: view.clone(),
                })
            }
            BotCommand::ConsumePrivateDecision {
                fence,
                dispatcher,
                operation,
                boot,
                ..
            } => {
                let lose = s.scripted("consume")?;
                assert_eq!(boot, s.boot);
                assert_eq!(Some(&fence), s.consumer.as_ref(), "only the consumer");
                let view = s.view.as_ref().expect("a decision");
                if !view.receipt.binding.matches_private(&operation) {
                    return Err(refusal("bot_conflict"));
                }
                let decision = view.receipt.decision;
                if let Some(key) = s.effects.get(&operation.operation_id) {
                    return Ok(BotReply::PrivateEffect {
                        effect: self.ledger.row(key),
                    });
                }
                let key = s.key(&fence, operation.operation_id);
                s.effects.insert(operation.operation_id, key.clone());
                let effect = self
                    .ledger
                    .open_keyed(key, operation, dispatcher, Some(decision));
                // Consumed: the wait no longer owns the Run once it settles.
                let state = &mut s.view.as_mut().expect("a decision").receipt.state;
                state.authorization = DecisionAuthorization::Consumed;
                state.phase = DecisionPhase::Settled;
                if lose {
                    return lost();
                }
                Ok(BotReply::PrivateEffect { effect })
            }
            _ => panic!("unexpected delegate command"),
        }
    }
}
