// SPDX-License-Identifier: MPL-2.0

//! Delegate IPC 18 through the real actor: the six private commands reach the
//! real orchestrator and the real in-memory ledger through `bot_command`, and
//! the decision path is driven by the actor's REAL maintenance tick (the
//! per-turn resume driver, the fixed publication worker, the cleanup owner),
//! as `decision_composition` drives an edit decision.
//!
//! Scripted composition, not provider qualification: the session host is the
//! builder's script, enrollment is the host's own bridge over a loopback
//! fixture, the clock is a number the test stores. No Store, vendor, keychain,
//! browser or disk. Every loop is bounded.

use super::decision_composition::{accept_within, decision_of, ledger, loop_once, steps};
use super::*;
use crate::decision_bridge::tests::{bridge, reply};
use koru_delegate_api::Failure;
use koru_delegate_api::bots::{
    BotCommand, BotReply, PrivateDispatcherId, PrivateEffectDisposition, PrivateEffectKey,
    PrivateEffectRefusal, PrivateEffectStage, PrivateEffectTransition, PrivateEffectView,
    PrivateOperationProposal,
};
use koru_delegate_core::decisions::{
    DecisionAuthorization, DecisionClosureReason, DecisionPhase, EnrollBotWaitCleanup,
};
use koru_delegate_core::{
    BotAdmissionState, BotBootScan, BotExecutionFence, BotOwner, BotRunKey, DelegateBootId,
    FreshBotPolicyProof,
};
use koru_delegate_runtime::DecisionResumeStep as Step;
use koru_delegate_runtime::test_support::{
    DecisionChain, GrantedPrivateRun, PreparedDecision, PublishedWaits, SessionScript,
    SettledEvents, bot_policy, granted_private_run, private_decision_chain, private_proposal,
    routine_private_run,
};
use koru_delegate_sqlite::PrivateEffectRecord;
use std::net::TcpListener;

// ---------------------------------------------------------------- helpers

fn effect(reply: Result<BotReply, Failure>) -> PrivateEffectView {
    match reply {
        Ok(BotReply::PrivateEffect { effect }) => effect,
        Ok(_) => panic!("another reply than a private effect"),
        Err(failure) => panic!("refused: {} ({})", failure.code, failure.message),
    }
}

/// The refusal's code, after checking what every `bot_` refusal promises:
/// not retryable, fixed text, no detail.
fn refused(reply: Result<BotReply, Failure>) -> String {
    let failure = match reply {
        Err(failure) => failure,
        Ok(_) => panic!("the command was not refused"),
    };
    if failure.code.starts_with("bot_") {
        assert!(!failure.retryable, "{}", failure.code);
        assert!(failure.details.is_empty(), "{}", failure.code);
    }
    failure.code
}

fn row(actor: &mut Actor, run: RunId, operation: uuid::Uuid) -> Option<PrivateEffectRecord> {
    actor
        .orchestrator
        .ports_mut()
        .store
        .private_effects()
        .expect("the private ledger is composed")
        .private_effect(run, operation)
        .unwrap()
}

fn stage(actor: &mut Actor, run: RunId, operation: uuid::Uuid) -> PrivateEffectStage {
    row(actor, run, operation).expect("the effect exists").stage
}

fn advance(
    actor: &mut Actor,
    key: &PrivateEffectKey,
    dispatcher: PrivateDispatcherId,
    transition: PrivateEffectTransition,
) -> Result<BotReply, Failure> {
    actor.bot_command(BotCommand::AdvancePrivateEffect {
        key: key.clone(),
        dispatcher,
        transition,
    })
}

fn read(
    actor: &mut Actor,
    run: RunId,
    operation: uuid::Uuid,
    dispatcher: PrivateDispatcherId,
) -> Result<BotReply, Failure> {
    actor.bot_command(BotCommand::ReadPrivateEffect {
        run,
        operation_id: operation,
        dispatcher,
    })
}

fn announce(actor: &mut Actor, owner: BotOwner, dispatcher: PrivateDispatcherId) -> u16 {
    match actor.bot_command(BotCommand::AnnouncePrivateDispatcher { owner, dispatcher }) {
        Ok(BotReply::PrivateDispatcher {
            dispatcher: answered,
            resolved,
        }) => {
            assert_eq!(answered, dispatcher);
            resolved
        }
        Ok(_) => panic!("another reply than the announce's"),
        Err(failure) => panic!("refused: {}", failure.code),
    }
}

const CONFIRMED: PrivateEffectTransition = PrivateEffectTransition::Confirmed {
    outcome_bytes: 321,
    withheld: false,
};

/// A Bot Run that holds the connection grant, inside the real actor.
struct Granted {
    actor: Actor,
    events: SettledEvents,
    waits: PublishedWaits,
    submission: koru_delegate_core::BotSubmission,
    owner: BotOwner,
    run: RunId,
    fence: BotExecutionFence,
    boot: DelegateBootId,
    proof: FreshBotPolicyProof,
    proposal: PrivateOperationProposal,
}

fn granted_from(mut built: GrantedPrivateRun) -> Granted {
    let events = SettledEvents::install(&mut built.orchestrator);
    let (commands, _inbox) = mpsc::channel();
    let actor = Actor::new(
        built.orchestrator,
        commands,
        built.owner.device.to_storage(),
    );
    Granted {
        actor,
        events,
        waits: built.waits,
        submission: built.submission,
        owner: built.owner,
        run: built.run,
        fence: built.fence,
        boot: built.boot,
        proof: built.proof,
        proposal: built.proposal,
    }
}

fn granted() -> Granted {
    granted_from(granted_private_run())
}

impl Granted {
    fn prepare(
        &mut self,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
    ) -> Result<BotReply, Failure> {
        self.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: self.fence.clone(),
            dispatcher,
            operation: proposal.clone(),
            proof: self.proof.clone(),
            boot: self.boot,
        })
    }
    fn hello(&mut self, scan: BotBootScan) {
        let reply = self.actor.bot_command(BotCommand::Hello {
            owner: self.owner,
            scan,
        });
        assert!(matches!(reply, Ok(BotReply::Hello { .. })));
    }
    /// A second read of the same granted connection, under a new id.
    fn another(&self) -> PrivateOperationProposal {
        let mut other = self.proposal.clone();
        other.operation_id = uuid::Uuid::new_v4();
        other
    }
}

// ------------------------------------------------------ the granted path

/// The five steps of one granted read, each lost reply replayed: one
/// `execute`, one `deliver`, one settled event, and a final row that admits
/// nothing more.
#[test]
fn m5_private_actor_a_granted_read_runs_once_and_every_lost_reply_replays() {
    let mut g = granted();
    let dispatcher = PrivateDispatcherId::mint();
    let proposal = g.proposal.clone();
    // Nothing exists yet: the read answers a positive Missing.
    assert!(matches!(
        read(&mut g.actor, g.run, proposal.operation_id, dispatcher),
        Ok(BotReply::Missing)
    ));
    // Intent: the one execution license.
    let prepared = effect(g.prepare(dispatcher, &proposal));
    assert_eq!(prepared.disposition, PrivateEffectDisposition::Execute);
    assert_eq!(prepared.stage, PrivateEffectStage::Prepared);
    assert_eq!(prepared.dispatcher, dispatcher);
    assert_eq!(prepared.decision, None);
    assert_eq!(prepared.operation, proposal);
    assert_eq!(
        (prepared.key.attempt, Some(prepared.key.turn)),
        (g.fence.attempt, g.fence.turn)
    );
    assert!(!prepared.deliver);
    // The reply was lost: the retry never gets a second license.
    for _ in 0..2 {
        let replay = effect(g.prepare(dispatcher, &proposal));
        assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
        assert_eq!(replay.stage, PrivateEffectStage::Prepared);
        assert_eq!(replay.key, prepared.key);
    }
    // A granted read asks nothing: no decision wait, no fence.
    assert_eq!(g.waits.get(g.run), None);
    // Dispatch mark, and its replay.
    for _ in 0..2 {
        let marked = effect(advance(
            &mut g.actor,
            &prepared.key,
            dispatcher,
            PrivateEffectTransition::Dispatched {},
        ));
        assert_eq!(marked.stage, PrivateEffectStage::Dispatched);
        assert!(!marked.deliver);
    }
    assert!(g.events.published().is_empty());
    // Receipt: content may be handed to the model on this one reply.
    let confirmed = effect(advance(&mut g.actor, &prepared.key, dispatcher, CONFIRMED));
    assert_eq!(confirmed.stage, PrivateEffectStage::Confirmed);
    assert_eq!(confirmed.outcome_bytes, Some(321));
    assert_eq!(confirmed.withheld, Some(false));
    assert!(confirmed.deliver);
    // The receipt's reply was lost: the replay records nothing and never
    // delivers a second time.
    let replay = effect(advance(&mut g.actor, &prepared.key, dispatcher, CONFIRMED));
    assert_eq!(replay.stage, PrivateEffectStage::Confirmed);
    assert!(!replay.deliver);
    // Final is final: no other transition, and no way back to `prepared`.
    for transition in [
        PrivateEffectTransition::Unknown {},
        PrivateEffectTransition::Dispatched {},
        PrivateEffectTransition::Refused {
            code: PrivateEffectRefusal::Unavailable,
        },
        PrivateEffectTransition::Confirmed {
            outcome_bytes: 1,
            withheld: false,
        },
    ] {
        assert_eq!(
            refused(advance(&mut g.actor, &prepared.key, dispatcher, transition)),
            "bot_private_effect_stale"
        );
    }
    let retained = effect(read(&mut g.actor, g.run, proposal.operation_id, dispatcher));
    assert_eq!(retained.stage, PrivateEffectStage::Confirmed);
    assert_eq!(
        retained.disposition,
        PrivateEffectDisposition::ReconcileOnly
    );
    assert!(!retained.deliver);
    let again = effect(g.prepare(dispatcher, &proposal));
    assert_eq!(
        (again.stage, again.disposition),
        (
            PrivateEffectStage::Confirmed,
            PrivateEffectDisposition::ReconcileOnly
        )
    );
    // One settled event, content-free.
    let published = g.events.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].1.outcome_bytes, Some(321));
    // A new read of the same connection is a new explicit operation.
    let next = g.another();
    let fresh = effect(g.prepare(dispatcher, &next));
    assert_eq!(fresh.disposition, PrivateEffectDisposition::Execute);
    assert_ne!(fresh.key.operation_id, prepared.key.operation_id);
}

/// Every refusal of the granted path and of the dispatcher's commands. A
/// non-retryable `bot_` failure means the command committed nothing.
#[test]
fn m5_private_actor_refusals_carry_their_code_and_commit_nothing() {
    let mut g = granted();
    let dispatcher = PrivateDispatcherId::mint();
    let proposal = g.proposal.clone();
    let none = |g: &mut Granted, operation: uuid::Uuid| {
        assert!(row(&mut g.actor, g.run, operation).is_none());
    };
    // Another device's owner.
    let mut foreign = g.fence.clone();
    foreign.owner.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: foreign,
            dispatcher,
            operation: proposal.clone(),
            proof: g.proof.clone(),
            boot: g.boot,
        })),
        "bot_owner_mismatch"
    );
    // A fingerprint that does not recompute.
    let mut forged = proposal.clone();
    forged.request_fingerprint = [5; 32];
    assert_eq!(refused(g.prepare(dispatcher, &forged)), "bot_invalid");
    // A boot that is not this delegate's.
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: g.fence.clone(),
            dispatcher,
            operation: proposal.clone(),
            proof: g.proof.clone(),
            boot: DelegateBootId::mint(),
        })),
        "bot_boot_stale"
    );
    // A fence that carries no current turn.
    let mut turnless = g.fence.clone();
    turnless.turn = None;
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: turnless,
            dispatcher,
            operation: proposal.clone(),
            proof: g.proof.clone(),
            boot: g.boot,
        })),
        "bot_turn_stale"
    );
    // A connection neither policy grants.
    let ungranted = {
        let mut other = private_proposal();
        other.connection = koru_delegate_core::ConnectionId::mint();
        other.request_fingerprint = other.fingerprint();
        other
    };
    assert_eq!(
        refused(g.prepare(dispatcher, &ungranted)),
        "bot_connection_not_granted"
    );
    none(&mut g, ungranted.operation_id);
    // The grant is held: no question is needed.
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::PreparePrivateDecision {
            fence: g.fence.clone(),
            operation: proposal.clone(),
            proof: g.proof.clone(),
            boot: g.boot,
        })),
        "bot_connection_granted"
    );
    none(&mut g, proposal.operation_id);
    assert_eq!(g.waits.get(g.run), None, "no wait was opened");
    // An advance or a read for a Run this ledger never admitted, and an
    // advance for an effect that does not exist.
    let key = PrivateEffectKey {
        run: g.run,
        operation_id: proposal.operation_id,
        attempt: g.fence.attempt,
        generation: g.fence.generation,
        turn: g.fence.turn.unwrap(),
    };
    let mut unknown_run = key.clone();
    unknown_run.run = RunId::mint();
    assert!(
        advance(
            &mut g.actor,
            &unknown_run,
            dispatcher,
            PrivateEffectTransition::Dispatched {}
        )
        .is_err()
    );
    assert!(
        read(
            &mut g.actor,
            unknown_run.run,
            proposal.operation_id,
            dispatcher
        )
        .is_err(),
        "an unknown Run is never a positive Missing"
    );
    assert_eq!(
        refused(advance(
            &mut g.actor,
            &key,
            dispatcher,
            PrivateEffectTransition::Dispatched {}
        )),
        "bot_private_effect_stale"
    );
    none(&mut g, proposal.operation_id);

    // From here one effect is open.
    let prepared = effect(g.prepare(dispatcher, &proposal));
    // A second operation of the caller's own dispatcher while it is open.
    let second = g.another();
    assert_eq!(
        refused(g.prepare(dispatcher, &second)),
        "bot_private_effect_open"
    );
    none(&mut g, second.operation_id);
    // The same operation id with another proposal.
    let mut changed = proposal.clone();
    changed.connection_revision = koru_delegate_core::decisions::DecisionRevision::new(9).unwrap();
    assert_eq!(refused(g.prepare(dispatcher, &changed)), "bot_conflict");
    // A key that names another attempt or turn of the same operation.
    let mut other_turn = prepared.key.clone();
    other_turn.turn = koru_delegate_core::AgentTurnId::mint();
    assert_eq!(
        refused(advance(
            &mut g.actor,
            &other_turn,
            dispatcher,
            PrivateEffectTransition::Dispatched {}
        )),
        "bot_fenced"
    );
    // A transition that is not legal from `prepared`.
    for early in [CONFIRMED, PrivateEffectTransition::Unknown {}] {
        assert_eq!(
            refused(advance(&mut g.actor, &prepared.key, dispatcher, early)),
            "bot_private_effect_stale"
        );
    }
    // None of it moved the open row.
    assert_eq!(
        stage(&mut g.actor, g.run, proposal.operation_id),
        PrivateEffectStage::Prepared
    );
    assert!(g.events.published().is_empty());
}

/// The rule for who may close an open row. `Hello`, in each of its scans,
/// never does; a command from the row's own dispatcher never does; a command
/// that carries another dispatcher id for the same owner does, before it does
/// anything else. A `prepared` row of a dead dispatcher is closed
/// `refused/not_dispatched`: the vendor was never called.
#[test]
fn m5_private_dispatcher_resolution_a_live_row_is_never_swept_and_an_announce_closes_a_dead_one() {
    let mut g = granted();
    let alive = PrivateDispatcherId::mint();
    let proposal = g.proposal.clone();
    let prepared = effect(g.prepare(alive, &proposal));
    let open = |g: &mut Granted| stage(&mut g.actor, g.run, proposal.operation_id);
    // The AI owner's probe, its `Start` walk and its own announce.
    for _ in 0..3 {
        g.hello(BotBootScan::Probe {});
        g.hello(BotBootScan::Start {});
    }
    assert_eq!(announce(&mut g.actor, g.owner, alive), 0);
    assert_eq!(announce(&mut g.actor, g.owner, alive), 0);
    // Every command of the row's own dispatcher.
    effect(read(&mut g.actor, g.run, proposal.operation_id, alive));
    effect(g.prepare(alive, &proposal));
    assert_eq!(open(&mut g), PrivateEffectStage::Prepared);
    // Another owner's announce does not reach this owner's rows; an announce
    // for an owner on another device is refused.
    let mut elsewhere = g.owner;
    elsewhere.device = koru_delegate_core::DeviceId::new();
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::AnnouncePrivateDispatcher {
            owner: elsewhere,
            dispatcher: PrivateDispatcherId::mint(),
        })),
        "bot_owner_mismatch"
    );
    assert_eq!(open(&mut g), PrivateEffectStage::Prepared);
    assert!(g.events.published().is_empty());

    // The gateway's process was replaced: its AI worker adopts the owner.
    let replacement = PrivateDispatcherId::mint();
    assert_eq!(announce(&mut g.actor, g.owner, replacement), 1);
    let closed = row(&mut g.actor, g.run, proposal.operation_id).unwrap();
    assert_eq!(closed.stage, PrivateEffectStage::Refused);
    assert_eq!(closed.refusal, Some(PrivateEffectRefusal::NotDispatched));
    assert_eq!(closed.dispatched_at_ms, None, "the vendor was never called");
    assert_eq!(closed.dispatcher, alive, "the row keeps its own dispatcher");
    // Idempotent: the announce is retried until acknowledged.
    assert_eq!(announce(&mut g.actor, g.owner, replacement), 0);
    // The old instance, if it is still there, learns its row is closed.
    assert_eq!(
        refused(advance(
            &mut g.actor,
            &prepared.key,
            alive,
            PrivateEffectTransition::Dispatched {}
        )),
        "bot_private_effect_stale"
    );
    let retry = effect(g.prepare(replacement, &proposal));
    assert_eq!(
        (retry.stage, retry.refusal, retry.disposition),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::NotDispatched),
            PrivateEffectDisposition::ReconcileOnly
        )
    );
    let published = g.events.published();
    assert_eq!(published.len(), 1, "one settled event for the closed row");
    assert_eq!(
        published[0].1.refusal,
        Some(PrivateEffectRefusal::NotDispatched)
    );
}

/// A `dispatched` row of a dead dispatcher ends `unknown`. That is final: a
/// late answer is dropped, nothing leads back to `prepared`, and nothing
/// resends it. Any later private command of the new instance closes it, not
/// only the announce.
#[test]
fn m5_private_dispatcher_resolution_an_in_flight_read_of_a_dead_dispatcher_ends_unknown_and_is_never_resent()
 {
    let mut g = granted();
    let dead = PrivateDispatcherId::mint();
    let proposal = g.proposal.clone();
    let prepared = effect(g.prepare(dead, &proposal));
    effect(advance(
        &mut g.actor,
        &prepared.key,
        dead,
        PrivateEffectTransition::Dispatched {},
    ));
    // The replacement's first private command is a read of another operation:
    // it resolves first, although it then finds nothing.
    let replacement = PrivateDispatcherId::mint();
    assert!(matches!(
        read(&mut g.actor, g.run, uuid::Uuid::new_v4(), replacement),
        Ok(BotReply::Missing)
    ));
    let closed = row(&mut g.actor, g.run, proposal.operation_id).unwrap();
    assert_eq!(closed.stage, PrivateEffectStage::Unknown);
    assert!(closed.dispatched_at_ms.is_some());
    assert_eq!(announce(&mut g.actor, g.owner, replacement), 0);
    // The read it was really performing answers late: stale, dropped.
    assert_eq!(
        refused(advance(&mut g.actor, &prepared.key, dead, CONFIRMED)),
        "bot_private_effect_stale"
    );
    // Not even the new instance may turn `unknown` into something else.
    for transition in [
        CONFIRMED,
        PrivateEffectTransition::Dispatched {},
        PrivateEffectTransition::Refused {
            code: PrivateEffectRefusal::Unavailable,
        },
    ] {
        assert_eq!(
            refused(advance(
                &mut g.actor,
                &prepared.key,
                replacement,
                transition
            )),
            "bot_private_effect_stale"
        );
    }
    // The exact retry reads the retained outcome and never a license.
    for _ in 0..2 {
        let retry = effect(g.prepare(replacement, &proposal));
        assert_eq!(
            (retry.stage, retry.disposition, retry.deliver),
            (
                PrivateEffectStage::Unknown,
                PrivateEffectDisposition::ReconcileOnly,
                false
            )
        );
    }
    assert_eq!(
        stage(&mut g.actor, g.run, proposal.operation_id),
        PrivateEffectStage::Unknown
    );
    let published = g.events.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].1.stage, PrivateEffectStage::Unknown);
    // A new call with a new operation id is a new, explicit operation.
    let next = g.another();
    assert_eq!(
        effect(g.prepare(replacement, &next)).disposition,
        PrivateEffectDisposition::Execute
    );
}

/// The revocation fence through the actor. A grant removed before the
/// dispatch mark turns the mark into `refused/access_denied`, answered as a
/// reply; a grant removed while the call is in flight lets the receipt be
/// recorded and withholds delivery.
#[test]
fn m5_private_actor_a_moved_barrier_refuses_the_dispatch_mark_as_a_reply_and_withholds_delivery() {
    // Removed before dispatch.
    let mut g = granted();
    let dispatcher = PrivateDispatcherId::mint();
    let proposal = g.proposal.clone();
    let prepared = effect(g.prepare(dispatcher, &proposal));
    let revoke = |g: &mut Granted| {
        let mut newer = bot_policy(&g.submission, 2);
        newer.policy_revision = 2;
        newer.policy.connections.clear();
        let reply = g
            .actor
            .bot_command(BotCommand::SynchronizePolicy { barrier: newer });
        assert!(matches!(reply, Ok(BotReply::PolicyBarrier { .. })));
    };
    revoke(&mut g);
    for _ in 0..2 {
        // The mark, and the replay of its lost reply.
        let fenced = effect(advance(
            &mut g.actor,
            &prepared.key,
            dispatcher,
            PrivateEffectTransition::Dispatched {},
        ));
        assert_eq!(
            (fenced.stage, fenced.refusal, fenced.deliver),
            (
                PrivateEffectStage::Refused,
                Some(PrivateEffectRefusal::AccessDenied),
                false
            )
        );
    }
    assert_eq!(
        row(&mut g.actor, g.run, proposal.operation_id)
            .unwrap()
            .dispatched_at_ms,
        None,
        "the vendor is never called"
    );
    assert_eq!(g.events.published().len(), 1);

    // Removed while the call is in flight.
    let mut g = granted();
    let proposal = g.proposal.clone();
    let prepared = effect(g.prepare(dispatcher, &proposal));
    effect(advance(
        &mut g.actor,
        &prepared.key,
        dispatcher,
        PrivateEffectTransition::Dispatched {},
    ));
    revoke(&mut g);
    let late = effect(advance(&mut g.actor, &prepared.key, dispatcher, CONFIRMED));
    assert_eq!(late.stage, PrivateEffectStage::Confirmed);
    assert!(!late.deliver, "the content is dropped");
}

/// A delegate restart before and after the dispatch mark. The delegate's own
/// start closes nothing; the row's dispatcher, which outlived it, records the
/// truth against the new process. Nothing leads back to `prepared`.
#[test]
fn m5_private_actor_a_delegate_restart_before_and_after_dispatch_leaves_the_row_to_its_dispatcher()
{
    for dispatched_before_restart in [false, true] {
        let GrantedPrivateRun {
            mut orchestrator,
            owner,
            run,
            fence,
            boot,
            proof,
            proposal,
            ..
        } = granted_private_run();
        let device = owner.device;
        let dispatcher = PrivateDispatcherId::mint();
        let prepared = orchestrator
            .bot_prepare_private_effect(&fence, device, dispatcher, &proposal, &proof, boot)
            .unwrap();
        if dispatched_before_restart {
            orchestrator
                .bot_advance_private_effect(
                    &prepared.key,
                    dispatcher,
                    &PrivateEffectTransition::Dispatched {},
                    device,
                )
                .unwrap();
        }
        // The delegate process is replaced over the same ledger.
        let mut orchestrator = koru_delegate_runtime::Orchestrator::new(
            orchestrator.into_ports(),
            koru_delegate_runtime::Options {
                reask_attempts: 3,
                ..Default::default()
            },
        );
        assert_ne!(orchestrator.delegate_boot(), boot);
        let events = SettledEvents::install(&mut orchestrator);
        let (commands, _inbox) = mpsc::channel();
        let mut actor = Actor::new(orchestrator, commands, device.to_storage());
        // Its start, its maintenance and the AI owner's probe close nothing.
        for _ in 0..3 {
            actor.maintenance_tick();
            let hello = actor.bot_command(BotCommand::Hello {
                owner,
                scan: BotBootScan::Probe {},
            });
            assert!(matches!(hello, Ok(BotReply::Hello { .. })));
        }
        let before = effect(read(&mut actor, run, proposal.operation_id, dispatcher));
        assert_eq!(
            before.stage,
            if dispatched_before_restart {
                PrivateEffectStage::Dispatched
            } else {
                PrivateEffectStage::Prepared
            }
        );
        assert_eq!(before.disposition, PrivateEffectDisposition::ReconcileOnly);
        // A prepare under the old boot is refused; the exact retry under any
        // boot only reads the retained row.
        let retry = effect(actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: fence.clone(),
            dispatcher,
            operation: proposal.clone(),
            proof: proof.clone(),
            boot,
        }));
        assert_eq!(retry.disposition, PrivateEffectDisposition::ReconcileOnly);
        assert_eq!(retry.stage, before.stage);
        // The same dispatcher finishes against the new delegate.
        let marked = effect(advance(
            &mut actor,
            &prepared.key,
            dispatcher,
            PrivateEffectTransition::Dispatched {},
        ));
        assert_eq!(marked.stage, PrivateEffectStage::Dispatched);
        let confirmed = effect(advance(&mut actor, &prepared.key, dispatcher, CONFIRMED));
        assert_eq!(confirmed.stage, PrivateEffectStage::Confirmed);
        assert!(confirmed.deliver);
        assert_eq!(events.published().len(), 1);
    }
}

/// D6: a Routine Run never has a private question. It needs a grant made in
/// advance, and its grant is the Routine's own.
#[test]
fn m5_private_actor_a_routine_run_never_asks_a_private_question() {
    let mut g = granted_from(routine_private_run());
    let proposal = g.proposal.clone();
    assert_eq!(
        refused(g.actor.bot_command(BotCommand::PreparePrivateDecision {
            fence: g.fence.clone(),
            operation: proposal.clone(),
            proof: g.proof.clone(),
            boot: g.boot,
        })),
        "bot_routine_decision_unsupported"
    );
    assert_eq!(g.waits.get(g.run), None, "no wait was opened");
    // The Routine's grant holds no connection: the read is refused, and no
    // question takes its place.
    assert_eq!(
        refused(g.prepare(PrivateDispatcherId::mint(), &proposal)),
        "bot_connection_not_granted"
    );
    assert!(row(&mut g.actor, g.run, proposal.operation_id).is_none());
}

// ------------------------------------------------------ the decision path

/// An actor whose private decision was allowed and resumed on its
/// acknowledged successor, through the real tick. Nothing is consumed yet.
struct Asked {
    actor: Actor,
    inbox: mpsc::Receiver<Command>,
    prepared: PreparedDecision,
    sessions: Arc<std::sync::Mutex<SessionScript>>,
    waits: PublishedWaits,
    events: SettledEvents,
    /// The acknowledged successor: the only fence that may consume.
    consumer: BotExecutionFence,
    creation: IdempotencyKey,
    proposal: PrivateOperationProposal,
}

fn asked() -> Asked {
    let DecisionChain {
        mut orchestrator,
        sessions,
        prepared,
        waits,
        ..
    } = private_decision_chain();
    let events = SettledEvents::install(&mut orchestrator);
    let owner = prepared.owner;
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    prepared.script_resume(&mut sessions.lock().unwrap(), successor, turn);
    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let answering = prepared.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(30));
        reply(&mut stream, |request: &EnrollBotWaitCleanup| {
            answering.enrolled_reply(request)
        });
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut allowed = false;
    while steps(&actor.resume_log).last() != Some(&Step::InputAcknowledged) {
        assert!(
            Instant::now() < deadline,
            "the chain did not reach the acknowledged successor: {:?}",
            steps(&actor.resume_log)
        );
        loop_once(&mut actor, &inbox);
        if !allowed && steps(&actor.resume_log).contains(&Step::OriginStopped) {
            prepared.allow_once(&mut actor.orchestrator).unwrap();
            allowed = true;
        }
    }
    server.join().unwrap();
    let keys = prepared
        .successor_keys(&mut actor.orchestrator)
        .unwrap()
        .unwrap();
    let consumer = BotExecutionFence {
        attempt: keys.attempt,
        generation: keys.generation,
        session: successor,
        turn: Some(turn),
        ..prepared.fence.clone()
    };
    let proposal = prepared.private.clone().expect("a private decision");
    Asked {
        actor,
        inbox,
        prepared,
        sessions,
        waits,
        events,
        consumer,
        creation: keys.creation,
        proposal,
    }
}

impl Asked {
    fn run(&self) -> RunId {
        self.prepared.run
    }
    fn consume_as(
        &mut self,
        fence: BotExecutionFence,
        dispatcher: PrivateDispatcherId,
        proposal: PrivateOperationProposal,
    ) -> Result<BotReply, Failure> {
        self.actor.bot_command(BotCommand::ConsumePrivateDecision {
            fence,
            dispatcher,
            operation: proposal,
            proof: self.prepared.proof.clone(),
            boot: self.prepared.boot,
        })
    }
    fn consume(&mut self, dispatcher: PrivateDispatcherId) -> Result<BotReply, Failure> {
        self.consume_as(self.consumer.clone(), dispatcher, self.proposal.clone())
    }
    /// (authorization, phase, closure reason) of the decision, from the ledger.
    fn decision(
        &mut self,
    ) -> (
        DecisionAuthorization,
        DecisionPhase,
        Option<DecisionClosureReason>,
    ) {
        let record = decision_of(&mut self.actor, &self.prepared);
        (
            record.receipt.state.authorization,
            record.receipt.state.phase,
            record.receipt.closure.map(|closure| closure.reason),
        )
    }
    fn wait_open(&mut self) -> bool {
        let (owner, run) = (self.prepared.owner, self.prepared.run);
        ledger(&mut self.actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_some()
    }
    fn admission(&mut self) -> BotAdmissionState {
        let run = self.prepared.run;
        ledger(&mut self.actor)
            .bot_execution(run)
            .unwrap()
            .unwrap()
            .state
    }
    /// The actor's own maintenance publishes the settlement's Control and
    /// settles the wait; the Run's classification is then republished.
    fn until_the_fence_lifts(&mut self) {
        let bound = Instant::now() + Duration::from_secs(30);
        while self.wait_open() {
            assert!(Instant::now() < bound, "the wait did not settle");
            loop_once(&mut self.actor, &self.inbox);
        }
        loop_once(&mut self.actor, &self.inbox);
        assert_eq!(
            self.waits.get(self.run()),
            None,
            "the Run no longer classifies as waiting"
        );
        assert!(!self.actor.orchestrator.run_publication_pending(self.run()));
    }
    /// A person's Stop of the resumed Run, on the private Bot route.
    fn stop(&mut self) {
        self.sessions
            .lock()
            .unwrap()
            .script_retirement(self.creation, CreationLookup::NoEvidence);
        let key = BotRunKey {
            owner: self.prepared.owner,
            bot: self.consumer.bot,
            run: self.run(),
        };
        // A decision-managed Stop answers that publication is still owed; the
        // actor's tick finishes it.
        let _ = self.actor.bot_command(BotCommand::Stop {
            key,
            fence: Some(self.consumer.clone()),
        });
    }
    fn until_settled(&mut self) {
        let bound = Instant::now() + Duration::from_secs(40);
        while self.admission() != BotAdmissionState::Settled {
            assert!(
                Instant::now() < bound,
                "the Run did not finish: decision {:?}",
                self.decision()
            );
            loop_once(&mut self.actor, &self.inbox);
        }
        loop_once(&mut self.actor, &self.inbox);
        assert!(!self.wait_open(), "the wait settled");
        assert_eq!(self.waits.get(self.run()), None, "the fence lifted");
        let device = self.prepared.owner.device;
        assert_eq!(
            ledger(&mut self.actor)
                .bot_execution_inventory(device)
                .unwrap()
                .occupied,
            0,
            "capacity released"
        );
    }
    /// The resumed Bot is an ordinary executing attempt.
    fn still_executing(&mut self) {
        assert_eq!(self.admission(), BotAdmissionState::Executing);
        let run = self.run();
        let row = self
            .actor
            .orchestrator
            .ports_mut()
            .store
            .current_attempt(run)
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.attempt, row.generation, row.retired),
            (self.consumer.attempt, self.consumer.generation, false)
        );
    }
}

/// The whole allowed read through the real actor: only the acknowledged
/// consumer consumes, once; the fence holds while the wait is open; the
/// confirmed read settles the decision at once and the actor lifts the fence
/// mid-turn while the Run keeps executing.
#[test]
fn m5_private_actor_an_allowed_read_is_consumed_once_and_a_confirmed_read_lifts_the_fence_mid_turn()
{
    let mut a = asked();
    let run = a.run();
    let dispatcher = PrivateDispatcherId::mint();
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Granted,
            DecisionPhase::AwaitRetry,
            None
        )
    );
    assert!(a.waits.get(run).is_some(), "the Run classifies as waiting");
    // The origin is not the consumer, a stale boot is not this delegate's,
    // and another proposal under the approved id is a conflict.
    let origin = a.prepared.fence.clone();
    assert!(
        a.consume_as(origin, dispatcher, a.proposal.clone())
            .is_err()
    );
    assert_eq!(
        refused(a.actor.bot_command(BotCommand::ConsumePrivateDecision {
            fence: a.consumer.clone(),
            dispatcher,
            operation: a.proposal.clone(),
            proof: a.prepared.proof.clone(),
            boot: DelegateBootId::mint(),
        })),
        "bot_boot_stale"
    );
    let mut changed = a.proposal.clone();
    changed.connection_revision = koru_delegate_core::decisions::DecisionRevision::new(7).unwrap();
    assert_eq!(
        refused(a.consume_as(a.consumer.clone(), dispatcher, changed)),
        "bot_conflict"
    );
    // An operation the person was never asked about has no decision.
    assert_eq!(
        refused(a.consume_as(a.consumer.clone(), dispatcher, private_proposal())),
        "bot_fenced"
    );
    assert!(row(&mut a.actor, run, a.proposal.operation_id).is_none());
    assert_eq!(a.decision().0, DecisionAuthorization::Granted);

    // The one-use consume.
    let consumed = effect(a.consume(dispatcher));
    assert_eq!(consumed.disposition, PrivateEffectDisposition::Execute);
    assert_eq!(consumed.stage, PrivateEffectStage::Prepared);
    assert_eq!(consumed.decision, Some(a.prepared.decision));
    assert_eq!(
        (
            consumed.key.attempt,
            consumed.key.generation,
            Some(consumed.key.turn)
        ),
        (a.consumer.attempt, a.consumer.generation, a.consumer.turn)
    );
    assert_eq!(
        a.decision(),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect, None)
    );
    // Its reply was lost: the retry reconciles and never executes again.
    for _ in 0..2 {
        let replay = effect(a.consume(dispatcher));
        assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
        assert_eq!(replay.key, consumed.key);
    }
    // O147: while the wait is open only the exact decision retry passes.
    assert_eq!(
        refused(a.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: a.consumer.clone(),
            dispatcher,
            operation: private_proposal(),
            proof: a.prepared.proof.clone(),
            boot: a.prepared.boot,
        })),
        "bot_decision_owned"
    );
    assert_eq!(
        refused(a.actor.bot_command(BotCommand::PreparePrivateDecision {
            fence: a.consumer.clone(),
            operation: private_proposal(),
            proof: a.prepared.proof.clone(),
            boot: a.prepared.boot,
        })),
        "bot_decision_owned"
    );
    assert!(a.waits.get(run).is_some());

    // Dispatch, then the receipt. The decision settles on that very command.
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Dispatched {},
    ));
    assert_eq!(a.decision().1, DecisionPhase::Effect);
    let confirmed = effect(advance(&mut a.actor, &consumed.key, dispatcher, CONFIRMED));
    assert!(confirmed.deliver);
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Completed)
        ),
        "settled on the confirmed read"
    );
    a.until_the_fence_lifts();
    a.still_executing();
    // The exact retry after the settlement reads the receipt: never a second
    // license, never a second delivery, and the decision is not reopened.
    let after = effect(a.consume(dispatcher));
    assert_eq!(
        (after.stage, after.disposition, after.deliver),
        (
            PrivateEffectStage::Confirmed,
            PrivateEffectDisposition::ReconcileOnly,
            false
        )
    );
    assert_eq!(a.decision().1, DecisionPhase::Settled);
    let published = a.events.published();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].1.decision, Some(a.prepared.decision));
    // The fence is down: the Bot's next private call is judged on its own
    // (here: it has no grant), no longer refused as owned by the decision.
    assert_eq!(
        refused(a.actor.bot_command(BotCommand::PreparePrivateEffect {
            fence: a.consumer.clone(),
            dispatcher,
            operation: private_proposal(),
            proof: a.prepared.proof.clone(),
            boot: a.prepared.boot,
        })),
        "bot_connection_not_granted"
    );
}

/// D16: a refused read settles the decision mid-turn. The fence lifts and
/// the resumed Bot keeps working; it does not wait for its turn to end.
#[test]
fn m5_private_settle_cleanup_a_refused_read_settles_the_decision_mid_turn_and_lifts_the_fence() {
    let mut a = asked();
    let dispatcher = PrivateDispatcherId::mint();
    let consumed = effect(a.consume(dispatcher));
    // The credential was locked: nothing was sent, refused from `prepared`.
    let refusal = effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Refused {
            code: PrivateEffectRefusal::CredentialLocked,
        },
    ));
    assert_eq!(refusal.stage, PrivateEffectStage::Refused);
    assert!(!refusal.deliver);
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::EffectNotApplied)
        ),
        "settled on the refusal, on that very command"
    );
    assert!(a.waits.get(a.run()).is_some(), "the Control is still owed");
    a.until_the_fence_lifts();
    a.still_executing();
    // The approval is spent: the retry answers the refusal, never a license.
    let retry = effect(a.consume(dispatcher));
    assert_eq!(
        (retry.stage, retry.refusal, retry.disposition),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::CredentialLocked),
            PrivateEffectDisposition::ReconcileOnly
        )
    );
    assert_eq!(a.events.published().len(), 1);
    // And a vendor refusal after the dispatch mark settles the same way.
    let mut a = asked();
    let consumed = effect(a.consume(dispatcher));
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Dispatched {},
    ));
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Refused {
            code: PrivateEffectRefusal::VendorRejected,
        },
    ));
    assert_eq!(
        a.decision().2,
        Some(DecisionClosureReason::EffectNotApplied)
    );
    a.until_the_fence_lifts();
    a.still_executing();
}

/// An `unknown` read does not settle the decision before cleanup: the fence
/// stays up however often the loop turns. A person's Stop then finishes, and
/// the decision closes `effect_unknown` with the receipt as its outcome.
#[test]
fn m5_private_settle_cleanup_an_unknown_read_settles_only_at_cleanup_and_a_stop_finishes() {
    let mut a = asked();
    let dispatcher = PrivateDispatcherId::mint();
    let consumed = effect(a.consume(dispatcher));
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Dispatched {},
    ));
    let unknown = effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Unknown {},
    ));
    assert_eq!(unknown.stage, PrivateEffectStage::Unknown);
    for _ in 0..6 {
        loop_once(&mut a.actor, &a.inbox);
    }
    assert_eq!(
        a.decision(),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect, None),
        "unknown never settles mid-turn"
    );
    assert!(a.waits.get(a.run()).is_some(), "the fence stays up");
    a.stop();
    a.until_settled();
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::EffectUnknown)
        )
    );
    // Final, and never resent: the retry reads `unknown`.
    assert_eq!(
        stage(&mut a.actor, a.prepared.run, a.proposal.operation_id),
        PrivateEffectStage::Unknown
    );
    assert_eq!(
        refused(advance(&mut a.actor, &consumed.key, dispatcher, CONFIRMED)),
        "bot_private_effect_stale"
    );
    assert_eq!(a.events.published().len(), 1);
}

/// Resolution settles exactly as an advance does. The gateway's process died
/// after the consume: the next announce closes its row, and a `prepared` row
/// (`refused/not_dispatched`) settles the decision at once, so the fence does
/// not stay up until the turn ends.
#[test]
fn m5_private_settle_cleanup_a_dead_dispatchers_prepared_row_settles_its_decision_at_the_announce()
{
    let mut a = asked();
    let dead = PrivateDispatcherId::mint();
    effect(a.consume(dead));
    for _ in 0..3 {
        loop_once(&mut a.actor, &a.inbox);
    }
    assert_eq!(a.decision().1, DecisionPhase::Effect, "nothing sweeps it");
    assert_eq!(
        announce(&mut a.actor, a.prepared.owner, PrivateDispatcherId::mint()),
        1
    );
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::EffectNotApplied)
        ),
        "the approval is spent; the vendor was never called"
    );
    a.until_the_fence_lifts();
    a.still_executing();
    let published = a.events.published();
    assert_eq!(published.len(), 1);
    assert_eq!(
        published[0].1.refusal,
        Some(PrivateEffectRefusal::NotDispatched)
    );
}

/// The same, after the dispatch mark: the row ends `unknown`, which waits for
/// cleanup, and the decision then closes `effect_unknown`.
#[test]
fn m5_private_settle_cleanup_a_dead_dispatchers_in_flight_row_ends_unknown_and_closes_at_cleanup() {
    let mut a = asked();
    let dead = PrivateDispatcherId::mint();
    let consumed = effect(a.consume(dead));
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dead,
        PrivateEffectTransition::Dispatched {},
    ));
    let replacement = PrivateDispatcherId::mint();
    assert_eq!(announce(&mut a.actor, a.prepared.owner, replacement), 1);
    assert_eq!(
        stage(&mut a.actor, a.prepared.run, a.proposal.operation_id),
        PrivateEffectStage::Unknown
    );
    for _ in 0..4 {
        loop_once(&mut a.actor, &a.inbox);
    }
    assert_eq!(a.decision().1, DecisionPhase::Effect, "waits for cleanup");
    assert!(a.waits.get(a.run()).is_some());
    // The replacement's exact retry reads the outcome; it is never resent.
    let retry = effect(a.consume(replacement));
    assert_eq!(
        (retry.stage, retry.disposition),
        (
            PrivateEffectStage::Unknown,
            PrivateEffectDisposition::ReconcileOnly
        )
    );
    a.stop();
    a.until_settled();
    assert_eq!(a.decision().2, Some(DecisionClosureReason::EffectUnknown));
}

/// A Stop while the dispatcher still owns an open row: cleanup waits, because
/// an open row is never reported as not applied. Once the dispatcher records
/// the truth, the Stop finishes and nothing is stranded.
#[test]
fn m5_private_settle_cleanup_a_stop_with_an_open_row_waits_for_its_dispatcher_then_finishes() {
    for dispatched in [false, true] {
        let mut a = asked();
        let dispatcher = PrivateDispatcherId::mint();
        let consumed = effect(a.consume(dispatcher));
        if dispatched {
            effect(advance(
                &mut a.actor,
                &consumed.key,
                dispatcher,
                PrivateEffectTransition::Dispatched {},
            ));
        }
        a.stop();
        let waiting = Instant::now() + Duration::from_millis(400);
        while Instant::now() < waiting {
            loop_once(&mut a.actor, &a.inbox);
        }
        assert_ne!(
            a.admission(),
            BotAdmissionState::Settled,
            "cleanup waits while the row is open"
        );
        assert!(a.wait_open());
        assert_ne!(a.decision().1, DecisionPhase::Settled);
        // The cleanup was requested and owns the wait; what it lacks is the
        // effect's outcome, so no final record was written.
        let (owner, run) = (a.prepared.owner, a.prepared.run);
        let execution = ledger(&mut a.actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .unwrap();
        assert_eq!(
            execution.wait.phase,
            koru_delegate_core::decisions::BotWaitPhase::CleanupPending
        );
        assert!(execution.wait.cleanup.is_none());
        assert!(
            !stage(&mut a.actor, a.prepared.run, a.proposal.operation_id).is_final(),
            "a Stop never closes an effect row"
        );
        // The dispatcher's call ends by its own deadline and it says so. The
        // consumer is gone, so whatever it read is dropped by its caller.
        let (transition, reason) = if dispatched {
            (
                PrivateEffectTransition::Unknown {},
                DecisionClosureReason::EffectUnknown,
            )
        } else {
            (
                PrivateEffectTransition::Refused {
                    code: PrivateEffectRefusal::Unavailable,
                },
                DecisionClosureReason::EffectNotApplied,
            )
        };
        effect(advance(&mut a.actor, &consumed.key, dispatcher, transition));
        a.until_settled();
        assert_eq!(
            a.decision(),
            (
                DecisionAuthorization::Consumed,
                DecisionPhase::Settled,
                Some(reason)
            )
        );
        assert_eq!(a.events.published().len(), 1);
    }
}

/// A Stop after the receipt and its settlement, before the settlement's
/// Control applied, finishes too (O152), with the read as the outcome.
#[test]
fn m5_private_settle_cleanup_a_stop_after_the_receipt_finishes_completed() {
    let mut a = asked();
    let dispatcher = PrivateDispatcherId::mint();
    let consumed = effect(a.consume(dispatcher));
    effect(advance(
        &mut a.actor,
        &consumed.key,
        dispatcher,
        PrivateEffectTransition::Dispatched {},
    ));
    effect(advance(&mut a.actor, &consumed.key, dispatcher, CONFIRMED));
    assert!(a.wait_open(), "the settlement's Control is still owed");
    a.stop();
    a.until_settled();
    assert_eq!(
        a.decision(),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Completed)
        )
    );
}

/// An approval the Bot never used. A person's Stop closes it with the
/// cleanup's own cause and no effect row. Its authorization is not `granted`
/// at the end (the ledger's check does not admit that): what stays readable
/// is the retained Allow answer and that it was never consumed, which is what
/// a card must read to say "not read".
#[test]
fn m5_private_settle_cleanup_an_approval_never_used_closes_without_an_effect_row() {
    let mut a = asked();
    a.stop();
    a.until_settled();
    let record = decision_of(&mut a.actor, &a.prepared);
    assert_eq!(
        (
            record.receipt.state.authorization,
            record.receipt.state.phase,
            record
                .receipt
                .closure
                .as_ref()
                .map(|closure| closure.reason)
        ),
        (
            DecisionAuthorization::Canceled,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Canceled)
        )
    );
    assert!(record.receipt.answer.is_some(), "the Allow is retained");
    assert!(row(&mut a.actor, a.prepared.run, a.proposal.operation_id).is_none());
    assert!(a.events.published().is_empty());
    // The spent wait admits no late consume and no cleanup request.
    assert!(a.consume(PrivateDispatcherId::mint()).is_err());
    assert!(row(&mut a.actor, a.prepared.run, a.proposal.operation_id).is_none());
}

/// O157 for a private decision, on the real actor loop. Prepared, its origin
/// stopped and held, and nobody answers: before the deadline nothing closes
/// it; once the clock passes it the actor's own tick expires it, the cleanup
/// owner finishes, the fence lifts and the Run ends interrupted with its
/// capacity released. No successor is made and no effect row is written.
#[test]
fn m5_private_actor_an_unanswered_private_decision_expires_at_its_deadline_on_the_real_tick() {
    use koru_delegate_core::RunStatus;
    let DecisionChain {
        orchestrator,
        sessions,
        prepared,
        waits,
        clock,
        ..
    } = private_decision_chain();
    let (owner, run) = (prepared.owner, prepared.run);
    let proposal = prepared.private.clone().unwrap();
    prepared.script_resume(
        &mut sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let answering = prepared.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(30));
        reply(&mut stream, |request: &EnrollBotWaitCleanup| {
            answering.enrolled_reply(request)
        });
    });
    let bound = Instant::now() + Duration::from_secs(30);
    while !steps(&actor.resume_log).contains(&Step::OriginStopped) {
        assert!(
            Instant::now() < bound,
            "the origin was not stopped: {:?}",
            steps(&actor.resume_log)
        );
        loop_once(&mut actor, &inbox);
    }
    server.join().unwrap();
    let deadline = decision_of(&mut actor, &prepared)
        .receipt
        .deadline
        .deadline_at_ms;

    // One millisecond short of the deadline: it keeps waiting, fence up.
    clock.store(deadline - 1, Ordering::SeqCst);
    for _ in 0..5 {
        loop_once(&mut actor, &inbox);
    }
    let waiting = decision_of(&mut actor, &prepared);
    assert_eq!(
        (
            waiting.receipt.state.authorization,
            waiting.receipt.state.phase
        ),
        (DecisionAuthorization::Pending, DecisionPhase::Held)
    );
    assert!(!steps(&actor.resume_log).contains(&Step::Expired));
    assert!(waits.get(run).is_some(), "the Run classifies as waiting");

    // The deadline passes with nobody answering.
    clock.store(deadline, Ordering::SeqCst);
    let bound = Instant::now() + Duration::from_secs(30);
    while ledger(&mut actor)
        .bot_execution(run)
        .unwrap()
        .unwrap()
        .state
        != BotAdmissionState::Settled
    {
        assert!(
            Instant::now() < bound,
            "the expired private decision did not end its Run: steps {:?}",
            steps(&actor.resume_log)
        );
        loop_once(&mut actor, &inbox);
    }
    loop_once(&mut actor, &inbox);
    let expired = decision_of(&mut actor, &prepared);
    assert_eq!(
        (
            expired.receipt.state.authorization,
            expired.receipt.state.phase,
            expired
                .receipt
                .closure
                .as_ref()
                .map(|closure| closure.reason),
            expired.receipt.answer.is_none()
        ),
        (
            DecisionAuthorization::Expired,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Expired),
            true
        )
    );
    assert_eq!(
        steps(&actor.resume_log)
            .iter()
            .filter(|step| **step == Step::Expired)
            .count(),
        1
    );
    assert!(
        ledger(&mut actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_none(),
        "the wait settled"
    );
    assert_eq!(waits.get(run), None, "the fence lifted");
    let last = ledger(&mut actor)
        .latest_applied_decision_projection(owner, run)
        .unwrap()
        .unwrap();
    assert!(
        matches!(last.target.effects().status, RunStatus::Interrupted { .. }),
        "{:?}",
        last.target.effects().status
    );
    assert_eq!(
        ledger(&mut actor)
            .bot_execution_inventory(owner.device)
            .unwrap()
            .occupied,
        0,
        "capacity released"
    );
    assert_eq!(
        sessions.lock().unwrap().created().len(),
        1,
        "only the origin"
    );
    // A late answer changes nothing, and no effect was ever written.
    assert!(prepared.allow_once(&mut actor.orchestrator).is_err());
    assert_eq!(decision_of(&mut actor, &prepared).receipt, expired.receipt);
    assert!(row(&mut actor, run, proposal.operation_id).is_none());
}

/// The actor refuses a malformed private command before anything reads the
/// ledger, and its new refusals carry their one fixed text.
#[test]
fn m5_private_actor_a_malformed_command_is_refused_first_and_failures_are_fixed_text() {
    let mut g = granted();
    let dispatcher = PrivateDispatcherId::mint();
    // A version-1 operation id never reaches the orchestrator.
    let v1 = uuid::Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
    assert_eq!(
        refused(read(&mut g.actor, g.run, v1, dispatcher)),
        "bot_invalid"
    );
    let mut key = PrivateEffectKey {
        run: g.run,
        operation_id: v1,
        attempt: g.fence.attempt,
        generation: g.fence.generation,
        turn: g.fence.turn.unwrap(),
    };
    assert_eq!(
        refused(advance(
            &mut g.actor,
            &key,
            dispatcher,
            PrivateEffectTransition::Unknown {}
        )),
        "bot_invalid"
    );
    // The new codes answer their fixed text, with no value in it.
    key.operation_id = g.proposal.operation_id;
    let failure = advance(
        &mut g.actor,
        &key,
        dispatcher,
        PrivateEffectTransition::Unknown {},
    )
    .unwrap_err();
    assert_eq!(failure.code, "bot_private_effect_stale");
    assert_eq!(
        failure.message,
        "The private operation was already resolved."
    );
    for needle in [
        g.run.to_string(),
        g.proposal.operation_id.to_string(),
        g.proposal.connection.to_string(),
    ] {
        assert!(!failure.message.contains(&needle));
        assert!(!format!("{failure:?}").contains(&needle));
    }
    let ungranted = {
        let mut other = private_proposal();
        other.connection = koru_delegate_core::ConnectionId::mint();
        other.request_fingerprint = other.fingerprint();
        other
    };
    let failure = g.prepare(dispatcher, &ungranted).unwrap_err();
    assert_eq!(failure.code, "bot_connection_not_granted");
    assert_eq!(
        failure.message,
        "This Bot does not have access to the connection."
    );
    assert!(!format!("{failure:?}").contains(&ungranted.connection.to_string()));
}
