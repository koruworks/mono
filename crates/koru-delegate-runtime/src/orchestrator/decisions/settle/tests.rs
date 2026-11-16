// SPDX-License-Identifier: MPL-2.0

//! O150: the approved edit's positively recorded save settles the decision,
//! the wait then settles on its ordinary path and the decision-wait fence
//! lifts mid-turn. Driven through the public decision-chain builder over the
//! real in-memory ledger and document staging, with the scripted session
//! host. Scripted runtime composition, not provider qualification: no Store,
//! provider, browser or disk.
//!
//! Both savers of an approved edit (Koru's own trigger and the model's exact
//! retry through the gateway) reach the delegate as the same commands: the
//! one-use `ConsumeDecisionEdit`, then the shared effect's
//! `ConfirmNativeResult`. These tests send exactly those, through the same
//! orchestrator entry points the host actor calls for them.

use super::*;
use crate::DecisionResumeStep as Step;
use crate::test_support::{
    DecisionChain, FIXTURE_NOW_MS, SessionCall, decision_chain, publish_owed,
};
use koru_delegate_core::decisions::{DecisionBindingV1, DecisionClosureReason};
use koru_delegate_core::{
    AgentTurnId, BotAdmissionState, ConfirmNativeResult, NativeResultDisposition, NativeResultKey,
    NativeResultKind, NativeResultObservation, NativeResultOperation, NativeResultReference,
    NativeResultRefusal, NativeResultStage, PrepareNativeResult, RunToolAuthority, SessionId,
};
use koru_delegate_sqlite::DecisionRecord;
use koru_delegate_sqlite::test_support as build;
use koru_session_protocol::CreationLookup;

/// A Koru edit decision the person allowed, whose successor's continuation
/// was acknowledged: the approved edit may now be consumed once.
struct Resumed {
    chain: DecisionChain,
    /// The acknowledged consumer's fence (the successor and its turn).
    consumer: BotExecutionFence,
    /// The successor's creation key, for its scripted retirement.
    creation: koru_delegate_core::IdempotencyKey,
    /// The one-use request the binding authorizes, keyed by the consumer.
    approved: PrepareNativeResult,
    payload: Vec<u8>,
}

fn resumed() -> Resumed {
    let mut chain = decision_chain();
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    chain
        .prepared
        .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);
    let mut bridge = chain.prepared.enrollment_bridge();
    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
    ] {
        let step = chain
            .orchestrator
            .bot_advance_decision_resume(owner, run, &mut bridge)
            .unwrap();
        assert_eq!(step, expected);
    }
    chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        let step = chain
            .orchestrator
            .bot_advance_decision_resume(owner, run, &mut bridge)
            .unwrap();
        assert_eq!(step, expected);
    }
    let keys = chain
        .prepared
        .successor_keys(&mut chain.orchestrator)
        .unwrap()
        .unwrap();
    let consumer = BotExecutionFence {
        attempt: keys.attempt,
        generation: keys.generation,
        session: keys.session.unwrap(),
        turn: Some(turn),
        ..chain.prepared.fence
    };
    let record = decision(&mut chain);
    let approved = build::consume_request(&record, &consumer).unwrap();
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let payload = binding.payload.as_slice().to_vec();
    Resumed {
        chain,
        consumer,
        creation: keys.creation,
        approved,
        payload,
    }
}

fn decision(chain: &mut DecisionChain) -> DecisionRecord {
    let (owner, run, id) = (
        chain.prepared.owner,
        chain.prepared.run,
        chain.prepared.decision,
    );
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(owner, run, id)
        .unwrap()
        .unwrap()
}

impl Resumed {
    fn device(&self) -> koru_delegate_core::DeviceId {
        self.chain.prepared.owner.device
    }
    fn run(&self) -> RunId {
        self.chain.prepared.run
    }

    /// The one-use consume, exactly as the shared effect sends it.
    fn consume(&mut self) -> koru_delegate_sqlite::NativeLeaseOutcome {
        let device = self.device();
        self.chain
            .orchestrator
            .bot_consume_decision_edit(
                &self.consumer,
                device,
                &self.approved,
                &self.payload,
                &self.chain.prepared.proof,
                self.chain.prepared.boot,
            )
            .unwrap()
    }

    /// The Store's positive outcome of the approved edit.
    fn durable(&self) -> ConfirmNativeResult {
        ConfirmNativeResult {
            key: self.approved.key.clone(),
            request_fingerprint: self.approved.request_fingerprint,
            observation: NativeResultObservation::Durable {
                evidence: NativeResultReference {
                    operation_id: self.approved.key.operation_id,
                    workspace: self.approved.destination,
                    document: self.approved.target.unwrap(),
                    version: vec![7, 7, 7],
                    title: "Approved page".into(),
                    confirmed_at_ms: FIXTURE_NOW_MS,
                    kind: NativeResultKind::EditedPage,
                },
            },
        }
    }

    fn observe(&self, observation: NativeResultObservation) -> ConfirmNativeResult {
        ConfirmNativeResult {
            key: self.approved.key.clone(),
            request_fingerprint: self.approved.request_fingerprint,
            observation,
        }
    }

    fn confirm(&mut self, request: &ConfirmNativeResult) -> NativeResultStage {
        let device = self.device();
        self.chain
            .orchestrator
            .confirm_native_result(request, device)
            .unwrap()
            .stage
    }

    /// An unrelated new page by the same consumer in the same turn.
    fn unrelated_page(&self) -> PrepareNativeResult {
        PrepareNativeResult {
            key: NativeResultKey {
                operation_id: uuid::Uuid::new_v4(),
                ..self.approved.key.clone()
            },
            kind: NativeResultOperation::CreatePage,
            destination: self.approved.destination,
            target: None,
            request_fingerprint: [5; 32],
            expected_revision: None,
            config_revision: self.approved.config_revision,
            policy_revision: self.approved.policy_revision,
            title: "Unrelated page".into(),
        }
    }

    fn prepare(
        &mut self,
        request: &PrepareNativeResult,
    ) -> Result<NativeResultDisposition, RunError> {
        let device = self.device();
        self.chain
            .orchestrator
            .prepare_native_effect(request, device)
            .map(|view| view.disposition)
    }

    /// What the Run's classification would publish now (a fresh sync).
    fn published(&mut self) -> Option<(DecisionId, DecisionPhase)> {
        self.chain.orchestrator.sync_decision_authorities().unwrap();
        self.chain.waits.get(self.run())
    }

    fn wait_open(&mut self) -> bool {
        let (owner, run) = (self.chain.prepared.owner, self.run());
        self.chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_some()
    }

    fn projections(&mut self) -> u64 {
        let (owner, run) = (self.chain.prepared.owner, self.run());
        self.chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .latest_decision_projection(owner, run)
            .unwrap()
            .map_or(0, |record| record.identity.sequence.get())
    }

    fn step(&mut self) -> Step {
        let (owner, run) = (self.chain.prepared.owner, self.run());
        let mut bridge = self.chain.prepared.enrollment_bridge();
        self.chain
            .orchestrator
            .bot_advance_decision_resume(owner, run, &mut bridge)
            .unwrap()
    }

    fn native_stage(&mut self) -> NativeResultStage {
        let key = self.approved.key.clone();
        self.chain
            .orchestrator
            .native_ledger()
            .unwrap()
            .native_result(&key)
            .unwrap()
            .unwrap()
            .stage
    }

    fn admission(&mut self) -> BotAdmissionState {
        let run = self.run();
        self.chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .bot_execution(run)
            .unwrap()
            .unwrap()
            .state
    }

    /// The owed Controls publish and the wait settles on its ordinary path,
    /// as the host actor's maintenance does after a publication completion.
    /// Whether the successor's ordinary stream was handed back to be pumped.
    fn publish_and_settle_wait(&mut self) -> bool {
        let run = self.run();
        assert!(publish_owed(&mut self.chain.orchestrator, run).unwrap() >= 1);
        self.chain
            .orchestrator
            .settle_publication_wait(run)
            .unwrap()
            .is_some()
    }
}

fn settled(record: &DecisionRecord) -> bool {
    record.receipt.state.authorization == DecisionAuthorization::Consumed
        && record.receipt.state.phase == DecisionPhase::Settled
        && record
            .receipt
            .closure
            .as_ref()
            .is_some_and(|closure| closure.reason == DecisionClosureReason::Completed)
}

#[test]
fn a_positive_save_by_korus_trigger_settles_the_decision_and_lifts_the_fence_mid_turn() {
    let mut r = resumed();
    let run = r.run();
    assert!(r.consume().execute.is_some(), "the one Execute lease");
    let unrelated = r.unrelated_page();
    // While the decision waits, unrelated work by the same Bot is refused.
    assert!(r.prepare(&unrelated).is_err());
    assert!(matches!(r.published(), Some((_, DecisionPhase::Effect))));

    // The approved edit's positive Store outcome is recorded. Its page
    // registration is owed while the decision holds the Run's handle.
    let stage = r.confirm(&r.durable());
    assert_eq!(stage, NativeResultStage::Verified);
    assert!(
        settled(&decision(&mut r.chain)),
        "settled on the positive save"
    );
    // Logical settlement alone does not end the wait: until its Control is
    // applied the Run still classifies as waiting and the ledger fences.
    assert_eq!(
        r.published(),
        Some((r.chain.prepared.decision, DecisionPhase::Settled))
    );
    assert!(r.prepare(&unrelated).is_err());

    assert!(
        r.publish_and_settle_wait(),
        "the live successor's ordinary stream resumes"
    );
    assert!(!r.wait_open(), "the wait settled on its ordinary path");
    assert!(!r.chain.orchestrator.run_publication_pending(run));
    assert_eq!(r.published(), None, "the fence is down");
    assert_eq!(
        r.native_stage(),
        NativeResultStage::Registered,
        "the save is registered once the handle is ordinary again"
    );

    // The resumed Bot keeps working in the same turn: its classification is
    // the plain Bot value (what the AI's Routine create and the gateway's
    // mutations consult), and a new page gets its own Execute lease.
    let device = r.device();
    let authority = r
        .chain
        .orchestrator
        .resolve_run_tool_authority(run, r.consumer.generation, device)
        .unwrap();
    assert!(matches!(authority, RunToolAuthority::Bot { .. }));
    assert_eq!(
        r.prepare(&unrelated).unwrap(),
        NativeResultDisposition::Execute
    );
    // An ordinary executing attempt: nothing was retired or settled.
    assert_eq!(r.admission(), BotAdmissionState::Executing);
    let row = r
        .chain
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.attempt, row.generation, row.retired),
        (r.consumer.attempt, r.consumer.generation, false)
    );
    assert_eq!(r.step(), Step::Idle, "no open wait is left to drive");
}

#[test]
fn a_positive_save_by_the_models_exact_retry_settles_the_decision_without_a_second_execute() {
    let mut r = resumed();
    // Koru's trigger consumed the edit but its reply was lost: it never ran
    // the Store effect or reported an outcome.
    assert!(r.consume().execute.is_some());
    // The model's exact retry replays the consume: reconcile only.
    assert!(r.consume().execute.is_none(), "never a second license");
    assert!(!settled(&decision(&mut r.chain)));
    // Its retained-intent recovery proves and records the Store outcome
    // through the same confirmation, which settles the decision.
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Verified);
    assert!(settled(&decision(&mut r.chain)));
    r.publish_and_settle_wait();
    assert_eq!(r.published(), None);
    assert_eq!(r.native_stage(), NativeResultStage::Registered);
    let unrelated = r.unrelated_page();
    assert_eq!(
        r.prepare(&unrelated).unwrap(),
        NativeResultDisposition::Execute
    );
    // With the wait settled no decision owns the Run: a further consume is
    // refused before any write, and the one save stays the only one.
    let device = r.device();
    let refused = r.chain.orchestrator.bot_consume_decision_edit(
        &r.consumer,
        device,
        &r.approved,
        &r.payload,
        &r.chain.prepared.proof,
        r.chain.prepared.boot,
    );
    assert!(matches!(refused, Err(RunError::Port(error)) if error.code == "bot_fenced"));
    assert_eq!(r.native_stage(), NativeResultStage::Registered);
}

#[test]
fn an_unknown_save_settles_nothing_and_keeps_the_fence() {
    let mut r = resumed();
    assert!(r.consume().execute.is_some());
    let before = r.projections();
    assert_eq!(
        r.confirm(&r.observe(NativeResultObservation::Unconfirmed)),
        NativeResultStage::Unconfirmed
    );
    let record = decision(&mut r.chain);
    assert_eq!(
        (
            record.receipt.state.authorization,
            record.receipt.state.phase
        ),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect)
    );
    assert_eq!(r.projections(), before, "nothing was recorded");
    assert!(matches!(r.published(), Some((_, DecisionPhase::Effect))));
    let unrelated = r.unrelated_page();
    assert!(r.prepare(&unrelated).is_err());
    // The driver does not settle an unresolved effect either.
    assert_eq!(r.step(), Step::Waiting("bot_consume_owned"));
    assert!(r.wait_open());
}

#[test]
fn a_refused_save_settles_nothing_and_the_cleanup_owns_the_wait_as_before() {
    let mut r = resumed();
    let run = r.run();
    assert!(r.consume().execute.is_some());
    let before = r.projections();
    assert_eq!(
        r.confirm(&r.observe(NativeResultObservation::NotApplied {
            code: NativeResultRefusal::Conflict,
        })),
        NativeResultStage::Refused
    );
    let record = decision(&mut r.chain);
    assert_eq!(
        (
            record.receipt.state.authorization,
            record.receipt.state.phase
        ),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect)
    );
    assert_eq!(r.projections(), before);
    assert_eq!(r.step(), Step::Waiting("bot_consume_owned"));
    assert!(matches!(r.published(), Some((_, DecisionPhase::Effect))));
    assert!(r.prepare(&r.unrelated_page()).is_err());

    // The existing cleanup then owns it: Stop retires the exact consumer and
    // records the final evidence with the refused outcome, as before O150.
    let (creation, consumer) = (r.creation, r.consumer.clone());
    r.chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(creation, CreationLookup::NoEvidence);
    let device = r.device();
    let key = koru_delegate_core::BotRunKey {
        owner: consumer.owner,
        bot: consumer.bot,
        run,
    };
    let _ = r.chain.orchestrator.bot_stop(&key, device, Some(&consumer));
    let _ = r.chain.orchestrator.bot_budget_tick(run);
    let owner = r.chain.prepared.owner;
    let execution = r
        .chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .decision_wait_execution(owner, run)
        .unwrap()
        .unwrap();
    assert!(matches!(
        execution.wait.cleanup,
        Some(koru_delegate_core::decisions::ConsumerCleanup::Confirmed {
            retained_effect_outcome: Some(
                koru_delegate_core::decisions::DecisionRetainedEffect::NotApplied { .. }
            ),
            ..
        })
    ));
    let record = decision(&mut r.chain);
    assert_eq!(record.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        record.receipt.closure.map(|closure| closure.reason),
        Some(DecisionClosureReason::EffectNotApplied)
    );
}

#[test]
fn the_settlement_is_idempotent_and_never_saves_twice() {
    let mut r = resumed();
    assert!(r.consume().execute.is_some());
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Verified);
    let settled_record = decision(&mut r.chain);
    assert!(settled(&settled_record));
    let after = r.projections();
    // A replayed confirmation (a lost reply, the model's repeat) records no
    // second settlement and no projection; the driver takes no step.
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Verified);
    let (owner, run) = (r.chain.prepared.owner, r.run());
    assert!(
        !r.chain
            .orchestrator
            .settle_saved_decision_effect(owner, run)
            .unwrap()
    );
    assert_eq!(r.step(), Step::Waiting("publication_pending"));
    assert_eq!(r.projections(), after);
    assert_eq!(decision(&mut r.chain).receipt, settled_record.receipt);
    // A replay of the consume licenses nothing.
    assert!(r.consume().execute.is_none());
    r.publish_and_settle_wait();
    // After the wait settled, a confirmation replay changes nothing either.
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Registered);
    assert!(!r.wait_open());
    assert_eq!(decision(&mut r.chain).receipt, settled_record.receipt);
}

#[test]
fn a_settlement_lost_after_the_save_is_taken_by_the_resume_driver() {
    let mut r = resumed();
    assert!(r.consume().execute.is_some());
    // The save is recorded, but the settlement that should follow it was
    // lost (a failed write, or the process stopped in between).
    let durable = r.durable();
    r.chain
        .orchestrator
        .native_ledger()
        .unwrap()
        .confirm_native_result(&durable)
        .unwrap();
    assert!(!settled(&decision(&mut r.chain)));
    assert_eq!(r.step(), Step::EffectSettled);
    assert!(settled(&decision(&mut r.chain)));
    assert_eq!(r.step(), Step::Waiting("publication_pending"));
    r.publish_and_settle_wait();
    assert_eq!(r.published(), None);
}

#[test]
fn a_person_stop_after_the_early_settlement_is_an_ordinary_stop() {
    let mut r = resumed();
    let run = r.run();
    assert!(r.consume().execute.is_some());
    r.confirm(&r.durable());
    r.publish_and_settle_wait();
    assert!(!r.wait_open());
    let consumer = r.consumer.clone();
    let device = r.device();
    let key = koru_delegate_core::BotRunKey {
        owner: consumer.owner,
        bot: consumer.bot,
        run,
    };
    let before = r.projections();
    let creation = r.creation;
    r.chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(creation, CreationLookup::NoEvidence);
    let calls = r.chain.sessions.lock().unwrap().calls().len();
    let stopped = r.chain.orchestrator.bot_stop(&key, device, Some(&consumer));
    // The ordinary Stop: it ends the successor's own session and retires its
    // creation, and succeeds. No decision cleanup is requested for the
    // settled wait: no Control, no reopened wait.
    assert!(stopped.is_ok(), "{stopped:?}");
    assert_eq!(
        r.chain.sessions.lock().unwrap().calls()[calls..],
        [
            SessionCall::Terminate(consumer.session),
            SessionCall::Retire(creation),
        ]
    );
    let row = r
        .chain
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert!(
        row.retired,
        "the successor's attempt is retired by the Stop"
    );
    assert_eq!(r.projections(), before);
    assert!(!r.wait_open());
    let record = decision(&mut r.chain);
    assert!(settled(&record), "the settled decision is never reopened");
}

#[test]
fn a_restart_after_the_early_settlement_recovers_with_the_fence_down_and_no_second_save() {
    for published in [false, true] {
        let mut r = resumed();
        let run = r.run();
        assert!(r.consume().execute.is_some());
        r.confirm(&r.durable());
        if published {
            r.publish_and_settle_wait();
        }
        // The delegate restarts over the same ledger and documents; the new
        // process's classification starts empty.
        let DecisionChain {
            orchestrator,
            sessions,
            clock,
            documents,
            waits,
            prepared,
        } = r.chain;
        let orchestrator = Orchestrator::new(
            orchestrator.into_ports(),
            crate::Options {
                reask_attempts: 3,
                ..Default::default()
            },
        );
        waits.clear();
        r.chain = DecisionChain {
            orchestrator,
            sessions,
            clock,
            documents,
            waits,
            prepared,
        };
        if !published {
            // The settlement survived; the owed Control is published by the
            // new process and the wait settles on its ordinary path.
            assert!(settled(&decision(&mut r.chain)));
            assert!(matches!(r.published(), Some((_, DecisionPhase::Settled))));
            r.publish_and_settle_wait();
        }
        assert!(!r.wait_open());
        assert_eq!(r.published(), None, "the fence is down after the restart");
        assert!(settled(&decision(&mut r.chain)));
        // The consumed edit's one lease is history: the ledger's consume
        // replay licenses nothing, and no second Store request exists.
        let record = decision(&mut r.chain);
        let replay = r
            .chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .consume_decision_edit(
                &build::expected(&record),
                DecisionTransitionId::mint(),
                &r.approved,
                &r.payload,
                &r.consumer,
                &r.chain.prepared.proof,
                r.chain.prepared.boot,
            );
        assert!(replay.is_ok_and(|outcome| outcome.execute.is_none()));
        assert_eq!(r.step(), Step::Idle);
        assert_eq!(r.admission(), BotAdmissionState::Executing);
        let row = r
            .chain
            .orchestrator
            .ports_mut()
            .store
            .current_attempt(run)
            .unwrap()
            .unwrap();
        assert!(!row.retired, "the successor's attempt is untouched");
    }
}

/// The delegate restarts over the same ledger, documents and session host;
/// the new process's published classification starts empty.
fn restarted(r: Resumed) -> Resumed {
    let Resumed {
        chain,
        consumer,
        creation,
        approved,
        payload,
    } = r;
    let DecisionChain {
        orchestrator,
        sessions,
        clock,
        documents,
        waits,
        prepared,
    } = chain;
    let orchestrator = Orchestrator::new(
        orchestrator.into_ports(),
        crate::Options {
            reask_attempts: 3,
            ..Default::default()
        },
    );
    waits.clear();
    Resumed {
        chain: DecisionChain {
            orchestrator,
            sessions,
            clock,
            documents,
            waits,
            prepared,
        },
        consumer,
        creation,
        approved,
        payload,
    }
}

impl Resumed {
    fn occupied(&mut self) -> usize {
        let device = self.device();
        self.chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .bot_execution_inventory(device)
            .unwrap()
            .occupied
    }

    /// The decision's cleanup, owned since a Stop or a process end arrived
    /// after the settlement and before the wait settled, finishes: the
    /// cleanup tick retires the consumer and records the final evidence
    /// carrying the Durable save, the owed Controls publish, the wait settles
    /// and the save is registered once; then the existing terminal cleanup
    /// settles the admission and releases capacity.
    fn finish_window_cleanup(&mut self) {
        let run = self.run();
        let owner = self.chain.prepared.owner;
        let _ = self.chain.orchestrator.bot_budget_tick(run);
        let execution = self
            .chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .decision_wait_execution(owner, run)
            .unwrap()
            .expect("the wait is still open until its Controls publish");
        assert!(
            matches!(
                execution.wait.cleanup,
                Some(koru_delegate_core::decisions::ConsumerCleanup::Confirmed {
                    retained_effect_outcome: Some(
                        koru_delegate_core::decisions::DecisionRetainedEffect::Durable { .. }
                    ),
                    ..
                })
            ),
            "the final cleanup carries the Durable save: {:?}",
            execution.wait.phase
        );
        assert!(settled(&decision(&mut self.chain)), "never reopened");
        // A later cleanup retry (the loop ticks again before the worker
        // publishes) must not supersede the still-unpublished final Control.
        let before = self.projections();
        assert!(
            self.chain
                .orchestrator
                .request_publication_cleanup(run, DecisionClosureReason::AuthorityChanged)
                .is_err()
        );
        assert_eq!(self.projections(), before, "no request after final cleanup");
        assert!(
            !self.publish_and_settle_wait(),
            "a retired consumer has no stream to resume"
        );
        assert!(!self.wait_open(), "the wait settled");
        assert_eq!(self.native_stage(), NativeResultStage::Registered);
        assert_eq!(self.published(), None);
        for _ in 0..4 {
            if self.admission() == BotAdmissionState::Settled {
                break;
            }
            let _ = self.chain.orchestrator.bot_budget_tick(run);
        }
        assert_eq!(self.admission(), BotAdmissionState::Settled);
        assert_eq!(self.occupied(), 0, "capacity released");
        // Exactly one intent for the approved operation, registered once.
        assert!(self.consume_after_wait().is_err());
        assert_eq!(self.native_stage(), NativeResultStage::Registered);
    }

    fn consume_after_wait(&mut self) -> Result<koru_delegate_sqlite::NativeLeaseOutcome, RunError> {
        let device = self.device();
        self.chain.orchestrator.bot_consume_decision_edit(
            &self.consumer,
            device,
            &self.approved,
            &self.payload,
            &self.chain.prepared.proof,
            self.chain.prepared.boot,
        )
    }
}

#[test]
fn a_stop_after_the_settlement_and_before_the_wait_settles_finishes() {
    let mut r = resumed();
    let run = r.run();
    assert!(r.consume().execute.is_some());
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Verified);
    assert!(settled(&decision(&mut r.chain)));
    assert!(r.wait_open(), "the settlement's Control is still owed");
    assert_eq!(r.occupied(), 1);
    let creation = r.creation;
    r.chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(creation, CreationLookup::NoEvidence);
    let consumer = r.consumer.clone();
    let device = r.device();
    let key = koru_delegate_core::BotRunKey {
        owner: consumer.owner,
        bot: consumer.bot,
        run,
    };
    // The decision-managed Stop: its cleanup is requested and the consumer
    // retired; publication is still owed.
    let stopped = r.chain.orchestrator.bot_stop(&key, device, Some(&consumer));
    assert!(
        matches!(&stopped, Err(RunError::Port(error)) if error.code == "publication_pending"),
        "{stopped:?}"
    );
    r.finish_window_cleanup();
}

#[test]
fn a_process_end_and_a_restart_after_the_settlement_recover_and_finish() {
    let mut r = resumed();
    let run = r.run();
    assert!(r.consume().execute.is_some());
    assert_eq!(r.confirm(&r.durable()), NativeResultStage::Verified);
    // The successor's process ended on its own before the wait settled: the
    // cleanup owner is requested as a positive native end requests it.
    assert!(
        r.chain
            .orchestrator
            .request_publication_cleanup(run, DecisionClosureReason::AuthorityChanged)
            .unwrap()
    );
    // The delegate restarts before anything else happened.
    let mut r = restarted(r);
    let creation = r.creation;
    r.chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(creation, CreationLookup::NoEvidence);
    assert!(matches!(r.published(), Some((_, DecisionPhase::Settled))));
    r.finish_window_cleanup();
}

/// M5 (D16): which retained outcome settles a consumed decision before its
/// turn ends. A private read settles on `confirmed` and on `refused`, never
/// on `unknown`; and no kind settles on another kind's outcome.
#[test]
fn m5_private_settle_cleanup_only_a_final_known_private_outcome_settles_mid_turn() {
    use koru_delegate_core::{PrivateEffectKey, PrivateEffectRefusal};
    let key = PrivateEffectKey {
        run: RunId::mint(),
        operation_id: uuid::Uuid::new_v4(),
        attempt: koru_delegate_core::RunAttemptId::mint(),
        generation: koru_delegate_core::AttemptGeneration::FIRST,
        turn: koru_delegate_core::AgentTurnId::mint(),
    };
    let confirmed = DecisionRetainedEffect::PrivateConfirmed {
        key: key.clone(),
        outcome_bytes: 9,
        withheld: false,
    };
    let withheld = DecisionRetainedEffect::PrivateConfirmed {
        key: key.clone(),
        outcome_bytes: 9,
        withheld: true,
    };
    let refused = DecisionRetainedEffect::PrivateNotApplied {
        key: key.clone(),
        code: PrivateEffectRefusal::CredentialLocked,
    };
    let unknown = DecisionRetainedEffect::PrivateUnknown { key };
    for settles in [&confirmed, &withheld, &refused] {
        assert!(super::settles_mid_turn(
            DecisionKind::PrivateOperation,
            settles
        ));
    }
    assert!(!super::settles_mid_turn(
        DecisionKind::PrivateOperation,
        &unknown
    ));
    for effect in [&confirmed, &withheld, &refused, &unknown] {
        assert!(!super::settles_mid_turn(DecisionKind::KoruEdit, effect));
        assert!(!super::settles_mid_turn(
            DecisionKind::NativeApproval,
            effect
        ));
    }
}
