// SPDX-License-Identifier: MPL-2.0

//! Scripted composition, not provider qualification. The runtime's
//! decision-chain builder hands this actor an orchestrator holding a prepared
//! Koru edit decision; the actor then drives it with its REAL maintenance
//! tick (`maintenance_tick`, the per-turn `decision_resume_turn`, the real
//! fixed publication worker and its `EnrollmentMailbox`), and every worker
//! wake is handled as the actor's loop handles it. Enrollment is answered by
//! the host's own `HostDecisionBridge` over a loopback HTTP fixture
//! (127.0.0.1, no disk) speaking the decision bridge envelope; the person's
//! AllowOnce arrives through the private Bot route (`AnswerDecision`). The
//! session host is the builder's script. No Store, provider, browser or disk.

use super::*;
use crate::actor::decision_resume::ResumeTurn;
use crate::decision_bridge::tests::{bridge, reply};
use koru_delegate_api::bots::{BotCommand, BotReply, DecisionAnswerRequest};
use koru_delegate_core::decisions::EnrollBotWaitCleanup;
use koru_delegate_runtime::DecisionResumeStep as Step;
use koru_delegate_runtime::test_support::{DecisionChain, decision_chain};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;

/// Accept the one enrollment connection, bounded so a missing request fails
/// the test instead of hanging it.
fn accept_within(listener: &TcpListener, within: Duration) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + within;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("bounded enrollment accept: {error}"),
        }
    }
}

fn steps(log: &[crate::actor::decision_resume::ResumeTurnRecord]) -> Vec<Step> {
    log.iter()
        .filter_map(|record| match record.turn {
            ResumeTurn::Stepped(_, step) => Some(step),
            _ => None,
        })
        .collect()
}

#[test]
fn scripted_composition_drives_a_prepared_edit_decision_to_one_acknowledged_successor_on_the_real_tick()
 {
    let DecisionChain {
        orchestrator,
        sessions,
        prepared,
        ..
    } = decision_chain();
    let (owner, run) = (prepared.owner, prepared.run);
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    prepared.script_resume(&mut sessions.lock().unwrap(), successor, turn);

    // The builder's orchestrator moves into the actor; the actor owns it now.
    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());

    // Enrollment over the host's own bridge and the loopback AI fixture,
    // started on the actor's fixed worker as the composed host does.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&requests);
    let answering = prepared.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(30));
        reply(&mut stream, |request: &EnrollBotWaitCleanup| {
            served.fetch_add(1, Ordering::SeqCst);
            answering.enrolled_reply(request)
        });
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut allowed = false;
    let (mut wakes, mut others) = (0usize, 0usize);
    loop {
        // Bounded: a stalled chain or a wake storm fails here with counts,
        // never hangs (the log itself is not printed; it can be long).
        assert!(
            Instant::now() < deadline,
            "the chain did not reach the acknowledged successor: steps {:?}, turns {}, wakes {wakes}, other commands {others}",
            steps(&actor.resume_log),
            actor.resume_log.len()
        );
        actor.maintenance_tick();
        // Every worker wake (publication completion or enrollment reply) is
        // handled as the actor's loop handles it, which is also a turn. The
        // drain is time-boxed so a steady stream of commands cannot starve
        // the deadline check above.
        let drain_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < drain_until {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            if matches!(command, Command::PublicationWake) {
                wakes += 1;
            } else {
                others += 1;
            }
            actor.handle(command);
        }
        let taken = steps(&actor.resume_log);
        if !allowed && taken.contains(&Step::OriginStopped) {
            // The person allows the edit once, on the private Bot route.
            let answer = prepared.allow_once_answer(&mut actor.orchestrator).unwrap();
            let reply = actor.bot_command(BotCommand::AnswerDecision {
                answer: DecisionAnswerRequest {
                    owner: answer.owner,
                    run: answer.run,
                    decision: answer.decision,
                    expected_revision: answer.expected_revision,
                    binding_digest: answer.binding_digest,
                    request: answer.request,
                    human: answer.human,
                    choice: answer.choice,
                    authority_digest: answer.authority_digest,
                },
                proof: prepared.proof.clone(),
                boot: prepared.boot,
            });
            assert!(matches!(reply, Ok(BotReply::Decision { .. })), "{reply:?}");
            allowed = true;
        }
        if taken.last() == Some(&Step::InputAcknowledged) {
            break;
        }
    }
    server.join().unwrap();

    // One exact step per turn, in the chain's order, all for this one Run.
    assert_eq!(
        steps(&actor.resume_log),
        vec![
            Step::EnrollmentRequested,
            Step::Enrolled,
            Step::OriginStopped,
            Step::SuccessorReserved,
            Step::SuccessorBound,
            Step::SuccessorInstalled,
            Step::InputAcknowledged,
        ]
    );
    assert!(actor.resume_log.iter().all(|record| match record.turn {
        ResumeTurn::Stepped(offered, _) | ResumeTurn::Unchanged(offered) => offered == run,
        ResumeTurn::Idle | ResumeTurn::Unavailable => true,
    }));
    // No enrollment job started on a turn that began with a handle out; one
    // job in all, and one HTTP request reached the AI fixture.
    assert!(
        actor
            .resume_log
            .iter()
            .filter(|record| record.handle_out)
            .all(|record| record.jobs_after == record.jobs_before),
        "{:?}",
        actor.resume_log
    );
    assert_eq!(
        actor
            .publication_worker
            .as_ref()
            .unwrap()
            .enrollments_started(),
        1
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    // The successor: created once under the reserved key, controller taken,
    // one continuation input under its key, acknowledged as the turn.
    let keys = prepared
        .successor_keys(&mut actor.orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(keys.session, Some(successor));
    assert_eq!(keys.generation.get(), prepared.fence.generation.get() + 1);
    {
        let script = sessions.lock().unwrap();
        assert_eq!(script.creations_under(keys.creation), 1);
        assert!(script.attached(successor));
        assert_eq!(script.inputs_with_key(keys.input), 1);
        assert_eq!(script.acknowledged_turn(keys.input), Some(turn));
    }
    // The ledger's acknowledged continuation names exactly that successor
    // and turn.
    let acknowledged = prepared
        .acknowledged_continuation(&mut actor.orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            acknowledged.decision,
            acknowledged.attempt,
            acknowledged.generation,
            acknowledged.session,
            acknowledged.turn
        ),
        (
            prepared.decision,
            keys.attempt,
            keys.generation,
            successor,
            turn
        )
    );
}

/// A Run whose publication the document keeps refusing is started once and
/// then waits one ordinary TICK, instead of restarting on every worker wake
/// (which spun the actor and kept the handle out). Meanwhile the handle is
/// home, so the resume driver still takes its steps, including enrollment,
/// which starts only while no handle is out.
#[test]
fn a_persistently_refused_publication_waits_a_tick_and_the_resume_driver_still_steps() {
    let DecisionChain {
        orchestrator,
        sessions,
        documents,
        prepared,
        ..
    } = decision_chain();
    let owner = prepared.owner;
    prepared.script_resume(
        &mut sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    // From here every preparation is refused, as a Store that keeps refusing.
    documents.refuse_preparations(Some("store_unavailable"));
    let before = documents.preparations();

    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let answering = prepared.clone();
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(10));
        reply(&mut stream, |request: &EnrollBotWaitCleanup| {
            answering.enrolled_reply(request)
        });
    });

    // A window shorter than one TICK, every wake handled as the loop does.
    let window = Instant::now() + TICK / 2;
    let mut wakes = 0usize;
    while Instant::now() < window {
        actor.maintenance_tick();
        while Instant::now() < window {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            if matches!(command, Command::PublicationWake) {
                wakes += 1;
            }
            actor.handle(command);
        }
    }
    // Counted before the fixture server is joined: a starved enrollment
    // never reaches it.
    let starts = documents.preparations() - before;
    assert_eq!(
        starts, 1,
        "one refused start, then the Run waits a TICK (wakes {wakes})"
    );
    assert_eq!(
        steps(&actor.resume_log),
        vec![
            Step::EnrollmentRequested,
            Step::Enrolled,
            Step::OriginStopped
        ]
    );
    // Enrollment was taken on a turn that began with the handle home.
    assert!(actor.resume_log.iter().any(|record| !record.handle_out
        && matches!(record.turn, ResumeTurn::Stepped(_, Step::Enrolled))));
    assert_eq!(
        actor
            .publication_worker
            .as_ref()
            .unwrap()
            .enrollments_started(),
        1
    );
    server.join().unwrap();
}

/// One actor turn as its loop runs it: a tick, then every wake, bounded.
fn turn_once(actor: &mut Actor, inbox: &mpsc::Receiver<Command>) {
    actor.maintenance_tick();
    let drain_until = Instant::now() + Duration::from_millis(50);
    while Instant::now() < drain_until {
        let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
            break;
        };
        actor.handle(command);
    }
}

fn ledger(actor: &mut Actor) -> &mut dyn koru_delegate_sqlite::BotLedger {
    actor.orchestrator.ports_mut().store.bots().unwrap()
}

/// An actor whose Bot decision was allowed, resumed on its acknowledged
/// successor, consumed and confirmed Durable through the private Bot route,
/// exactly as the shared native effect sends both (for Koru's own trigger and
/// for the model's exact retry alike). The decision is settled; its Control
/// is still owed, so the wait is still open.
struct Saved {
    actor: Actor,
    inbox: mpsc::Receiver<Command>,
    prepared: koru_delegate_runtime::test_support::PreparedDecision,
    sessions: Arc<std::sync::Mutex<koru_delegate_runtime::test_support::SessionScript>>,
    waits: koru_delegate_runtime::test_support::PublishedWaits,
    consumer: koru_delegate_core::BotExecutionFence,
    creation: IdempotencyKey,
    request: koru_delegate_core::PrepareNativeResult,
    payload: koru_delegate_core::decisions::DecisionPayload,
    /// The one instant the orchestrator and the ledger read.
    clock: Arc<std::sync::atomic::AtomicI64>,
}

fn saved() -> Saved {
    saved_from(decision_chain(), None)
}

/// [`saved`] over `chain` (a manual or a Routine Run). With `answer_before`
/// the person's answer arrives that many milliseconds before the decision's
/// deadline, and the clock stays there through the consume and the save.
fn saved_from(chain: DecisionChain, answer_before: Option<i64>) -> Saved {
    use koru_delegate_core::decisions::{
        DecisionAuthorization, DecisionBindingV1, DecisionClosureReason, DecisionPhase,
    };
    use koru_delegate_core::{
        BotExecutionFence, ConfirmNativeResult, NativeResultDisposition, NativeResultKey,
        NativeResultKind, NativeResultObservation, NativeResultOperation, NativeResultReference,
        PrepareNativeResult,
    };
    let DecisionChain {
        orchestrator,
        sessions,
        prepared,
        waits,
        clock,
        ..
    } = chain;
    let (owner, run) = (prepared.owner, prepared.run);
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
        turn_once(&mut actor, &inbox);
        if !allowed && steps(&actor.resume_log).contains(&Step::OriginStopped) {
            if let Some(before) = answer_before {
                let at = ledger(&mut actor)
                    .bot_decision(owner, run, prepared.decision)
                    .unwrap()
                    .unwrap()
                    .receipt
                    .deadline
                    .deadline_at_ms
                    - before;
                clock.store(at, Ordering::SeqCst);
            }
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
        ..prepared.fence
    };
    let record = ledger(&mut actor)
        .bot_decision(owner, run, prepared.decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run,
            attempt: consumer.attempt,
            generation: consumer.generation,
            turn,
            operation_id: binding.operation_id,
        },
        kind: NativeResultOperation::EditPage,
        destination: binding.workspace,
        target: Some(binding.target),
        request_fingerprint: binding.edit_fingerprint,
        expected_revision: Some(binding.expected_revision.as_slice().to_vec()),
        config_revision: binding.global_policy.config_revision.get(),
        policy_revision: binding.global_policy.policy_revision.get(),
        title: "Approved page".into(),
    };
    let consumed = actor.bot_command(BotCommand::ConsumeDecisionEdit {
        fence: consumer.clone(),
        request: request.clone(),
        payload: binding.payload.clone(),
        proof: prepared.proof.clone(),
        boot: prepared.boot,
    });
    assert!(
        matches!(&consumed, Ok(BotReply::NativeResult { intent })
            if intent.disposition == NativeResultDisposition::Execute),
        "{consumed:?}"
    );
    let confirmed = actor.bot_command(BotCommand::ConfirmNativeResult {
        request: ConfirmNativeResult {
            key: request.key.clone(),
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::Durable {
                evidence: NativeResultReference {
                    operation_id: binding.operation_id,
                    workspace: binding.workspace,
                    document: binding.target,
                    version: vec![7, 7, 7],
                    title: "Approved page".into(),
                    confirmed_at_ms: koru_delegate_runtime::test_support::FIXTURE_NOW_MS,
                    kind: NativeResultKind::EditedPage,
                },
            },
        },
    });
    assert!(
        matches!(&confirmed, Ok(BotReply::NativeResult { .. })),
        "{confirmed:?}"
    );
    let decided = ledger(&mut actor)
        .bot_decision(owner, run, prepared.decision)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            decided.receipt.state.authorization,
            decided.receipt.state.phase,
            decided.receipt.closure.map(|closure| closure.reason)
        ),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Completed)
        ),
        "settled on the confirmed save"
    );
    let payload = binding.payload.clone();
    Saved {
        actor,
        inbox,
        prepared,
        sessions,
        waits,
        consumer,
        creation: keys.creation,
        request,
        payload,
        clock,
    }
}

impl Saved {
    fn native_stage(&mut self) -> koru_delegate_core::NativeResultStage {
        self.actor
            .orchestrator
            .ports_mut()
            .store
            .native_results()
            .unwrap()
            .native_result(&self.request.key)
            .unwrap()
            .unwrap()
            .stage
    }

    /// The model's exact retry of the consume never gets a second Execute.
    fn replay_saves_nothing(&mut self) {
        let replay = self.actor.bot_command(BotCommand::ConsumeDecisionEdit {
            fence: self.consumer.clone(),
            request: self.request.clone(),
            payload: self.payload.clone(),
            proof: self.prepared.proof.clone(),
            boot: self.prepared.boot,
        });
        assert!(
            !matches!(&replay, Ok(BotReply::NativeResult { intent })
                if intent.disposition == koru_delegate_core::NativeResultDisposition::Execute),
            "{replay:?}"
        );
    }
}

/// O150 on the real actor loop: the decision settles on the confirmation,
/// the actor's own maintenance publishes the owed Control and settles the
/// wait, and the Run's published classification is no longer waiting, all
/// while the successor keeps running.
#[test]
fn a_confirmed_save_settles_the_decision_and_the_actor_lifts_the_fence_mid_turn() {
    use koru_delegate_core::{BotAdmissionState, NativeResultStage};
    let mut s = saved();
    let (owner, run) = (s.prepared.owner, s.prepared.run);
    // The actor's own maintenance publishes the owed Control, settles the
    // wait and republishes the Run's classification.
    let deadline = Instant::now() + Duration::from_secs(30);
    while ledger(&mut s.actor)
        .decision_wait_execution(owner, run)
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < deadline, "the wait did not settle");
        turn_once(&mut s.actor, &s.inbox);
    }
    turn_once(&mut s.actor, &s.inbox);
    assert_eq!(
        s.waits.get(run),
        None,
        "the Run no longer classifies as waiting"
    );
    assert!(!s.actor.orchestrator.run_publication_pending(run));
    assert_eq!(s.native_stage(), NativeResultStage::Registered);
    // The successor is an ordinary executing attempt.
    assert_eq!(
        ledger(&mut s.actor)
            .bot_execution(run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    let row = s
        .actor
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.attempt, row.generation, row.retired),
        (s.consumer.attempt, s.consumer.generation, false)
    );
    s.replay_saves_nothing();
}

/// O152 on the real actor loop: a person's Stop arriving after the
/// settlement but before its Control applied finishes instead of staying on
/// Stopping. The actor retires the successor, records the final cleanup with
/// the Durable save, publishes, settles the wait, registers the save once and
/// settles the admission, releasing its capacity.
#[test]
fn a_stop_after_the_settlement_finishes_on_the_real_tick() {
    use koru_delegate_core::{BotAdmissionState, BotRunKey, NativeResultStage};
    let mut s = saved();
    let (owner, run) = (s.prepared.owner, s.prepared.run);
    assert!(
        ledger(&mut s.actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_some(),
        "the settlement's Control is still owed"
    );
    s.sessions.lock().unwrap().script_retirement(
        s.creation,
        koru_session_protocol::CreationLookup::NoEvidence,
    );
    let stopped = s.actor.bot_command(BotCommand::Stop {
        key: BotRunKey {
            owner,
            bot: s.consumer.bot,
            run,
        },
        fence: Some(s.consumer.clone()),
    });
    // The decision-managed Stop answers that publication is still owed.
    assert!(stopped.is_err(), "{stopped:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while ledger(&mut s.actor)
        .bot_execution(run)
        .unwrap()
        .unwrap()
        .state
        != BotAdmissionState::Settled
    {
        assert!(
            Instant::now() < deadline,
            "the Stop did not finish: wait {:?}",
            ledger(&mut s.actor)
                .decision_wait_execution(owner, run)
                .unwrap()
                .map(|execution| (execution.wait.phase, execution.wait.cleanup.is_some()))
        );
        turn_once(&mut s.actor, &s.inbox);
        // The loop's own Bot budget deadline, as `run` serves it: every
        // budget-tracked Run gets its tick (cleanup and terminal settlement).
        for tracked in s.actor.orchestrator.bot_budget_runs() {
            let _ = s.actor.orchestrator.bot_budget_tick(tracked);
        }
    }
    assert!(
        ledger(&mut s.actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_none(),
        "the wait settled"
    );
    assert_eq!(s.native_stage(), NativeResultStage::Registered);
    assert_eq!(s.waits.get(run), None);
    let device = owner.device;
    assert_eq!(
        ledger(&mut s.actor)
            .bot_execution_inventory(device)
            .unwrap()
            .occupied,
        0,
        "capacity released"
    );
    s.replay_saves_nothing();
}

// ------------------------------------------------------------------ O157

fn decision_of(
    actor: &mut Actor,
    prepared: &koru_delegate_runtime::test_support::PreparedDecision,
) -> koru_delegate_sqlite::DecisionRecord {
    ledger(actor)
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap()
}

/// One loop iteration as `run` serves it: the maintenance tick with its
/// wakes, then the Bot budget deadline for every Run it tracks (the cleanup
/// owner's retry lives there).
fn loop_once(actor: &mut Actor, inbox: &mpsc::Receiver<Command>) {
    turn_once(actor, inbox);
    for tracked in actor.orchestrator.bot_budget_runs() {
        let _ = actor.orchestrator.bot_budget_tick(tracked);
    }
}

/// O157 on the real actor loop. A Koru edit decision is prepared, its origin
/// is stopped and held, and nobody answers. Before the deadline nothing
/// closes it. Once the clock passes the deadline, the actor's own maintenance
/// tick expires it, the existing cleanup owner records the final cleanup and
/// publishes it, the wait settles, the decision-wait fence lifts, and the Run
/// ends interrupted (the "Needed you" closing, not a failure) with its
/// capacity released.
#[test]
fn an_unanswered_edit_decision_expires_at_its_deadline_on_the_real_tick() {
    use koru_delegate_core::decisions::{
        DecisionAuthorization, DecisionClosureReason, DecisionPhase,
    };
    use koru_delegate_core::{BotAdmissionState, RunStatus};
    let DecisionChain {
        orchestrator,
        sessions,
        prepared,
        waits,
        clock,
        ..
    } = decision_chain();
    let (owner, run) = (prepared.owner, prepared.run);
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

    // One millisecond short of the deadline, however often the loop turns:
    // the decision keeps waiting for its person, fence up.
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
            "the expired decision did not end its Run: steps {:?}, wait {:?}",
            steps(&actor.resume_log),
            ledger(&mut actor)
                .decision_wait_execution(owner, run)
                .unwrap()
                .map(|execution| (execution.wait.phase, execution.wait.cleanup.is_some()))
        );
        loop_once(&mut actor, &inbox);
    }
    loop_once(&mut actor, &inbox);

    // The decision: expired, settled with that reason, asked for once.
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
    // The wait: settled. The fence: lifted.
    assert!(
        ledger(&mut actor)
            .decision_wait_execution(owner, run)
            .unwrap()
            .is_none(),
        "the wait settled"
    );
    assert_eq!(
        waits.get(run),
        None,
        "the Run no longer classifies as waiting"
    );
    assert!(!actor.orchestrator.run_publication_pending(run));
    // The Run: ended interrupted, never failed, and no successor was made.
    let last = ledger(&mut actor)
        .latest_applied_decision_projection(owner, run)
        .unwrap()
        .unwrap();
    assert!(
        matches!(last.target.effects().status, RunStatus::Interrupted { .. }),
        "{:?}",
        last.target.effects().status
    );
    assert_eq!(last.target.effects().failure_reason, None);
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
    // A late answer changes nothing.
    assert!(prepared.allow_once(&mut actor.orchestrator).is_err());
    assert_eq!(decision_of(&mut actor, &prepared).receipt, expired.receipt);
}

/// An answer accepted one millisecond before the deadline wins: its edit is
/// consumed and saved under that clock, the decision settles as Completed,
/// and when the clock then runs far past the deadline the actor neither
/// expires nor reopens it; the resumed Run keeps executing.
#[test]
fn an_answer_just_before_the_deadline_wins_and_the_settled_decision_is_never_reopened() {
    use koru_delegate_core::decisions::{DecisionClosureReason, DecisionPhase};
    use koru_delegate_core::{BotAdmissionState, NativeResultStage};
    let mut s = saved_from(decision_chain(), Some(1));
    let (owner, run) = (s.prepared.owner, s.prepared.run);
    let settled = decision_of(&mut s.actor, &s.prepared);
    let deadline = settled.receipt.deadline.deadline_at_ms;
    assert_eq!(
        settled
            .receipt
            .answer
            .as_ref()
            .map(|answer| answer.answered_at_ms),
        Some(deadline - 1)
    );
    // An hour past the deadline, with the settlement's Control still owed.
    s.clock.store(deadline + 3_600_000, Ordering::SeqCst);
    let bound = Instant::now() + Duration::from_secs(30);
    while ledger(&mut s.actor)
        .decision_wait_execution(owner, run)
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < bound, "the wait did not settle");
        loop_once(&mut s.actor, &s.inbox);
    }
    for _ in 0..3 {
        loop_once(&mut s.actor, &s.inbox);
    }
    assert!(!steps(&s.actor.resume_log).contains(&Step::Expired));
    let after = decision_of(&mut s.actor, &s.prepared);
    assert_eq!(after.receipt, settled.receipt, "never reopened");
    assert_eq!(after.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        after.receipt.closure.as_ref().map(|closure| closure.reason),
        Some(DecisionClosureReason::Completed)
    );
    assert_eq!(s.waits.get(run), None);
    assert_eq!(s.native_stage(), NativeResultStage::Registered);
    assert_eq!(
        ledger(&mut s.actor)
            .bot_execution(run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    let row = s
        .actor
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.attempt, row.generation, row.retired),
        (s.consumer.attempt, s.consumer.generation, false)
    );
    s.replay_saves_nothing();
}

/// After a delegate restart the elapsed wait is unproven: the new process
/// accepts no answer for the decision, and the actor's first maintenance
/// turns close it as clock-uncertain although its saved deadline is still
/// minutes away. The cleanup owner then finishes it from the ledger alone.
#[test]
fn a_decision_left_open_across_a_delegate_restart_is_closed_on_the_real_tick() {
    use koru_delegate_core::RunStatus;
    use koru_delegate_core::decisions::{
        DecisionAuthorization, DecisionClosureReason, DecisionPhase,
    };
    use koru_delegate_runtime::test_support::publish_owed;
    let DecisionChain {
        mut orchestrator,
        sessions,
        prepared,
        waits,
        clock,
        ..
    } = decision_chain();
    let (owner, run) = (prepared.owner, prepared.run);
    prepared.script_resume(
        &mut sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    // The first process stops and holds the origin, then goes away.
    let mut enrollment = prepared.enrollment_bridge();
    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
    ] {
        assert_eq!(
            orchestrator
                .bot_advance_decision_resume(owner, run, &mut enrollment)
                .unwrap(),
            expected
        );
    }
    publish_owed(&mut orchestrator, run).unwrap();
    let orchestrator = koru_delegate_runtime::Orchestrator::new(
        orchestrator.into_ports(),
        koru_delegate_runtime::Options {
            reask_attempts: 3,
            ..Default::default()
        },
    );
    assert_ne!(orchestrator.delegate_boot(), prepared.boot);
    // One minute later; the saved deadline is nine minutes away.
    clock.store(
        koru_delegate_runtime::test_support::FIXTURE_NOW_MS + 60_000,
        Ordering::SeqCst,
    );
    sessions.lock().unwrap().script_retirement(
        prepared.origin_creation,
        koru_session_protocol::CreationLookup::NoEvidence,
    );
    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    assert!(prepared.allow_once(&mut actor.orchestrator).is_err());

    let bound = Instant::now() + Duration::from_secs(30);
    while ledger(&mut actor)
        .decision_wait_execution(owner, run)
        .unwrap()
        .is_some()
    {
        assert!(
            Instant::now() < bound,
            "the restarted delegate did not close the decision: steps {:?}, wait {:?}",
            steps(&actor.resume_log),
            ledger(&mut actor)
                .decision_wait_execution(owner, run)
                .unwrap()
                .map(|execution| (execution.wait.phase, execution.wait.cleanup.is_some()))
        );
        loop_once(&mut actor, &inbox);
    }
    loop_once(&mut actor, &inbox);
    let closed = decision_of(&mut actor, &prepared);
    assert_eq!(
        (
            closed.receipt.state.authorization,
            closed.receipt.state.phase,
            closed
                .receipt
                .closure
                .as_ref()
                .map(|closure| closure.reason)
        ),
        (
            DecisionAuthorization::Expired,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::ClockUncertain)
        )
    );
    assert_eq!(
        steps(&actor.resume_log)
            .iter()
            .filter(|step| **step == Step::Expired)
            .count(),
        1
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
        sessions.lock().unwrap().created().len(),
        1,
        "only the origin"
    );
}

/// P4.3 section 4.10 on the real actor loop: a Routine Run's page edit
/// outside its grant is a decision bound to the Routine; the person allows
/// it; the successor of the same Routine occurrence consumes it once under
/// the Routine's grant; the confirmed save settles the decision and the
/// actor lifts the fence while the Run keeps executing.
#[test]
fn a_routine_runs_approved_edit_is_saved_once_and_settles_on_the_real_tick() {
    use koru_delegate_core::decisions::DecisionBindingV1;
    use koru_delegate_core::{BotAdmissionState, NativeResultStage};
    use koru_delegate_runtime::test_support::routine_decision_chain;
    let mut s = saved_from(routine_decision_chain(), None);
    let (owner, run) = (s.prepared.owner, s.prepared.run);
    let routine = s
        .prepared
        .submission
        .provenance
        .routine
        .expect("a Routine Run");
    let record = decision_of(&mut s.actor, &s.prepared);
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    assert_eq!(
        binding.routine.as_ref().map(|subject| subject.provenance),
        Some(routine)
    );
    assert_eq!(
        binding
            .routine_policy
            .as_ref()
            .map(|policy| policy.schedule),
        Some(routine.schedule)
    );
    // The lease the edit ran under names the Routine's grant.
    let intent = s
        .actor
        .orchestrator
        .ports_mut()
        .store
        .native_results()
        .unwrap()
        .native_result(&s.request.key)
        .unwrap()
        .unwrap();
    assert_eq!(
        intent
            .lease
            .as_ref()
            .and_then(|lease| lease.routine.as_ref())
            .map(|grant| grant.schedule),
        Some(routine.schedule)
    );
    let bound = Instant::now() + Duration::from_secs(30);
    while ledger(&mut s.actor)
        .decision_wait_execution(owner, run)
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < bound, "the wait did not settle");
        loop_once(&mut s.actor, &s.inbox);
    }
    loop_once(&mut s.actor, &s.inbox);
    assert_eq!(s.waits.get(run), None);
    assert_eq!(s.native_stage(), NativeResultStage::Registered);
    let admitted = ledger(&mut s.actor).bot_admission(run).unwrap().unwrap();
    assert_eq!(admitted.state, BotAdmissionState::Executing);
    assert_eq!(admitted.submission.provenance.routine, Some(routine));
    s.replay_saves_nothing();
}
