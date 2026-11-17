// SPDX-License-Identifier: MPL-2.0

//! A child-request builder for composition tests (M6): a parent Bot Run
//! executing its first turn, the Bots it may ask, and the ask itself, built
//! the way the AI owner builds it. Everything goes through public
//! orchestrator methods; no ledger row is written directly.
//!
//! The recipients' admission barriers are installed, but their boot is not
//! replayed, so an admitted child stays queued until a test stops it, times
//! it out or replays the recipient's boot. Scripted runtime composition, not
//! provider qualification.

use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};

use koru_delegate_core::children::{
    BotChildWaitEnrollmentReply, ChildPolicyRef, ChildRequest, ChildRequestBindingV1,
    ChildRequestItem, EnrollBotChildWaitCleanup, child_task_digest,
};
use koru_delegate_core::decisions::{BotWaitId, DecisionValidate, DecisionVersionV1};
use koru_delegate_core::{
    ActorRef, AgentTurnId, BotExecutionFence, BotId, BotOwner, BotRequestId, BotSubmission,
    BotWorkingFolder, ChildParent, ChildRequestId, DelegateBootId, HostInstanceId, IdempotencyKey,
    ProfileId, RunId, SessionId, WorkspaceScope,
};
use koru_session_protocol::CreationLookup;

use super::{
    DocumentFaults, Live, PublishedWaits, SessionScript, admission_barrier, bot_submission,
    fixture_uuid, live,
};
use crate::{ChildEnrollmentBridge, ChildRequestFact, ChildWaitMark, Orchestrator, PortResult};

/// The child waits published for each Run's tool classification, exactly as
/// the host's projection would receive them.
#[derive(Clone, Default)]
pub struct PublishedChildWaits(
    pub(super) Arc<Mutex<std::collections::HashMap<RunId, ChildWaitMark>>>,
);

impl PublishedChildWaits {
    /// The child wait last published for `run`, if its classification waits.
    pub fn get(&self, run: RunId) -> Option<ChildWaitMark> {
        self.0
            .lock()
            .expect("the published child waits are not poisoned")
            .get(&run)
            .copied()
    }
}

/// A bus that keeps every child-request fact it was handed, with the event
/// id it travelled under, and nothing else.
#[derive(Clone, Default)]
pub struct ChildEvents {
    published: Arc<Mutex<Vec<(koru_delegate_core::EventId, ChildRequestFact)>>>,
}

impl ChildEvents {
    /// Replace `orchestrator`'s event port with a new recorder and return it.
    pub fn install(orchestrator: &mut Orchestrator) -> Self {
        let recorder = Self::default();
        orchestrator.ports_mut().events = Box::new(recorder.clone());
        recorder
    }
    /// Every child-request fact published so far, in order.
    pub fn published(&self) -> Vec<(koru_delegate_core::EventId, ChildRequestFact)> {
        self.published
            .lock()
            .expect("the recorder is not poisoned")
            .clone()
    }
}

impl crate::RunEvents for ChildEvents {
    fn publish(
        &mut self,
        _event: koru_delegate_core::EventId,
        _fact: &crate::RunFact,
    ) -> Result<(), crate::PublishFailure> {
        Ok(())
    }
    fn publish_child_request(
        &mut self,
        event: koru_delegate_core::EventId,
        fact: &ChildRequestFact,
    ) -> Result<(), crate::PublishFailure> {
        self.published
            .lock()
            .expect("the recorder is not poisoned")
            .push((event, *fact));
        Ok(())
    }
}

/// A child enrollment bridge answering `Enrolled` for every request with the
/// parent admission's AI attempt id, and counting them.
#[derive(Clone)]
pub struct ScriptedChildEnrollment {
    ai_attempt: uuid::Uuid,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    refusing: Arc<std::sync::atomic::AtomicBool>,
}

impl ScriptedChildEnrollment {
    /// How many enrollment requests reached this bridge.
    pub fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// While set, every request fails as the bridge being down.
    pub fn refuse(&self, refusing: bool) {
        self.refusing
            .store(refusing, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ChildEnrollmentBridge for ScriptedChildEnrollment {
    fn enroll_child(
        &mut self,
        request: &EnrollBotChildWaitCleanup,
    ) -> PortResult<BotChildWaitEnrollmentReply> {
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.refusing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::PortError::new(
                crate::Port::Sessions,
                "decision_enrollment_unknown",
                "the fixture bridge is down",
            ));
        }
        Ok(BotChildWaitEnrollmentReply::Enrolled {
            receipt_digest: request.key.receipt_digest,
            key: request.key.clone(),
            ai_attempt: self.ai_attempt,
        })
    }
}

/// A parent Bot Run executing its first turn with nothing asked yet, and the
/// ports a test keeps reading after the orchestrator moved into an actor.
pub struct ChildChain {
    pub orchestrator: Orchestrator,
    pub sessions: Arc<Mutex<SessionScript>>,
    /// The one instant both the orchestrator and the ledger read.
    pub clock: Arc<AtomicI64>,
    pub documents: DocumentFaults,
    pub waits: PublishedWaits,
    pub child_waits: PublishedChildWaits,
    pub parent: ParentRun,
}

/// The identities of the parent Run and the helpers that build its asks.
#[derive(Clone)]
pub struct ParentRun {
    pub submission: BotSubmission,
    pub owner: BotOwner,
    pub bot: BotId,
    pub run: RunId,
    /// The origin consumer's fence, with its acknowledged InitialSend turn.
    pub fence: BotExecutionFence,
    pub boot: DelegateBootId,
    pub human: ProfileId,
    pub ai_attempt: uuid::Uuid,
    pub host: HostInstanceId,
    pub origin_creation: IdempotencyKey,
    /// The fresh proof of the parent's current policy (with the Routine's
    /// grant for a Routine Run).
    pub proof: koru_delegate_core::FreshBotPolicyProof,
}

/// A parent interactive Bot Run ready to ask ([`super::decision_chain`]'s
/// steps up to the synchronized policy, with no decision prepared).
pub fn child_chain() -> ChildChain {
    chain(live(false, None))
}

/// The same for a **Routine Run** ([`super::routine_decision_chain`]'s
/// admission), with nothing asked.
pub fn routine_child_chain() -> ChildChain {
    chain(live(true, None))
}

/// A parent interactive Bot Run whose captured policy **holds the
/// connection grant** of [`super::private_proposal`], with nothing asked.
pub fn granted_child_chain() -> ChildChain {
    chain(live(false, Some(super::private_proposal().grant())))
}

fn chain(live: Live) -> ChildChain {
    let origin_creation = live.origin_creation();
    let Live {
        orchestrator,
        sessions,
        clock,
        documents,
        waits,
        child_waits,
        submission,
        fence,
        boot,
        host,
        proof,
        ..
    } = live;
    let ActorRef::Person { profile: human } = submission.request.requested_by else {
        panic!("the fixture submission is a person's request");
    };
    let parent = ParentRun {
        owner: submission.provenance.owner,
        bot: submission.provenance.bot,
        run: submission.request.run,
        fence,
        boot,
        human,
        ai_attempt: submission
            .provenance
            .ai_attempt_id
            .expect("the fixture admission has an AI attempt"),
        host,
        origin_creation,
        proof,
        submission,
    };
    ChildChain {
        orchestrator,
        sessions,
        clock,
        documents,
        waits,
        child_waits,
        parent,
    }
}

/// The Bot a fixture recipient `n` (1..=9) is.
pub fn recipient_bot(n: u8) -> BotId {
    BotId::from_uuid(fixture_uuid(0x70 + n))
}

impl ParentRun {
    /// The child submission the AI owner would build for recipient `n` under
    /// this parent: a version 1 submission of the recipient Bot, for the same
    /// person and workspace, in the recipient's own folder, with
    /// `provenance.parent` set and the parent's policy (no connection grant
    /// the parent lacks). Pure data.
    pub fn child_submission(&self, n: u8, child_request: ChildRequestId) -> BotSubmission {
        let mut submission = bot_submission(0x20 + n);
        submission.request.run = RunId::mint();
        submission.request.requested_by = self.submission.request.requested_by.clone();
        submission.request.scope = WorkspaceScope::BotFolder {
            folder: BotWorkingFolder::new(format!("C:/fixture/recipient-{n}"))
                .expect("a fixture recipient folder"),
        };
        submission.provenance.bot = recipient_bot(n);
        submission.provenance.submission = BotRequestId::from_uuid(child_request.as_uuid());
        submission.provenance.chat_id = uuid::Uuid::new_v4();
        submission.provenance.ai_attempt_id = Some(uuid::Uuid::new_v4());
        submission.provenance.assistant_message_id = Some(uuid::Uuid::new_v4());
        submission.provenance.parent = Some(ChildParent {
            run: self.run,
            bot: self.bot,
            chat_id: self.submission.provenance.chat_id,
            child_request,
        });
        submission.policy = self.submission.policy.clone();
        submission.policy.connections.clear();
        submission.input.idempotency_key = IdempotencyKey::mint();
        submission
    }

    /// One ask of the recipients `ns`, in that order, under a fresh ask key
    /// and wait id, bound to this parent's current fence. Each recipient's
    /// unpaused admission barrier is installed first (the ledger refuses a
    /// recipient it holds no barrier for). Panics if the fixture is invalid.
    pub fn ask(&self, orchestrator: &mut Orchestrator, ns: &[u8]) -> ChildRequest {
        self.ask_with(orchestrator, ns, |_| {})
    }

    /// [`Self::ask`], with each child submission edited before it is bound.
    pub fn ask_with(
        &self,
        orchestrator: &mut Orchestrator,
        ns: &[u8],
        edit: impl Fn(&mut BotSubmission),
    ) -> ChildRequest {
        let wait = BotWaitId::mint();
        let ask_key = uuid::Uuid::new_v4();
        let parent = &self.submission;
        let parent_policy = ChildPolicyRef::of(
            parent.provenance.config_revision,
            parent.provenance.policy_revision,
            &parent.policy,
        )
        .expect("the parent policy reference");
        let requests = ns
            .iter()
            .enumerate()
            .map(|(index, n)| {
                let child_request = ChildRequestId::from_uuid(uuid::Uuid::new_v4());
                let mut submission = self.child_submission(*n, child_request);
                edit(&mut submission);
                orchestrator
                    .bot_barrier(&admission_barrier(&submission), self.owner.device)
                    .expect("install the recipient's admission barrier");
                let binding = ChildRequestBindingV1 {
                    version: DecisionVersionV1,
                    owner: self.owner,
                    parent_bot: self.bot,
                    parent_run: self.run,
                    wait,
                    ask_key,
                    ordinal: index as u8,
                    parent_submission: if parent.provenance.routine.is_some() {
                        koru_delegate_core::ROUTINE_BOT_SUBMISSION_VERSION
                    } else {
                        koru_delegate_core::INTERACTIVE_BOT_SUBMISSION_VERSION
                    },
                    parent_routine: parent.provenance.routine,
                    human: self.human,
                    recipient_bot: submission.provenance.bot,
                    child_run: submission.request.run,
                    child_request,
                    workspace: submission.workspace,
                    task_digest: child_task_digest(submission.request.objective.as_str())
                        .expect("the task digest"),
                    documents: vec![],
                    parent_policy,
                    child_policy: ChildPolicyRef::of(
                        submission.provenance.config_revision,
                        submission.provenance.policy_revision,
                        &submission.policy,
                    )
                    .expect("the child policy reference"),
                };
                ChildRequestItem {
                    child_request,
                    binding,
                    submission,
                }
            })
            .collect();
        ChildRequest {
            parent: self.fence.clone(),
            ask_key,
            human: self.human,
            requests,
        }
    }

    /// Whether `request` is a well-formed ask (the wire's own shape check).
    pub fn well_formed(request: &ChildRequest) -> bool {
        request.validate().is_ok()
    }

    /// A bridge that enrolls every child wait of this parent.
    pub fn enrollment_bridge(&self) -> ScriptedChildEnrollment {
        ScriptedChildEnrollment {
            ai_attempt: self.ai_attempt,
            requests: Default::default(),
            refusing: Default::default(),
        }
    }

    /// Queue on `script` everything the resume chain asks of the session
    /// host: the origin creation's retirement answered `NoEvidence`, the
    /// successor's creation as `successor`, and the continuation input's
    /// positive acknowledgment as `turn`.
    pub fn script_resume(
        &self,
        script: &mut SessionScript,
        successor: SessionId,
        turn: AgentTurnId,
    ) {
        script.script_retirement(self.origin_creation, CreationLookup::NoEvidence);
        script.script_creation(successor);
        script.script_turn(turn);
    }
}
