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

/// O150 on the real actor loop: once the person allowed the edit and the
/// successor's continuation was acknowledged, the approved edit is consumed
/// and its positive Store outcome confirmed through the private Bot route,
/// exactly as the shared native effect sends both (for Koru's own trigger
/// and for the model's exact retry alike). The decision settles on that
/// confirmation, the actor's own maintenance publishes the owed Control and
/// settles the wait, and the Run's published classification is no longer
/// waiting, all while the successor keeps running.
#[test]
fn a_confirmed_save_settles_the_decision_and_the_actor_lifts_the_fence_mid_turn() {
    use koru_delegate_core::decisions::{
        DecisionAuthorization, DecisionBindingV1, DecisionClosureReason, DecisionPhase,
    };
    use koru_delegate_core::{
        BotAdmissionState, BotExecutionFence, ConfirmNativeResult, NativeResultDisposition,
        NativeResultKey, NativeResultKind, NativeResultObservation, NativeResultOperation,
        NativeResultReference, NativeResultStage, PrepareNativeResult,
    };
    let DecisionChain {
        orchestrator,
        sessions,
        prepared,
        waits,
        ..
    } = decision_chain();
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
    // One actor turn as its loop runs it: a tick, then every wake, bounded.
    let turn_once = |actor: &mut Actor| {
        actor.maintenance_tick();
        let drain_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < drain_until {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            actor.handle(command);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut allowed = false;
    while steps(&actor.resume_log).last() != Some(&Step::InputAcknowledged) {
        assert!(
            Instant::now() < deadline,
            "the chain did not reach the acknowledged successor: {:?}",
            steps(&actor.resume_log)
        );
        turn_once(&mut actor);
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
        ..prepared.fence
    };
    fn ledger(actor: &mut Actor) -> &mut dyn koru_delegate_sqlite::BotLedger {
        actor.orchestrator.ports_mut().store.bots().unwrap()
    }
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

    // The actor's own maintenance publishes the owed Control, settles the
    // wait and republishes the Run's classification.
    let deadline = Instant::now() + Duration::from_secs(30);
    while ledger(&mut actor)
        .decision_wait_execution(owner, run)
        .unwrap()
        .is_some()
    {
        assert!(Instant::now() < deadline, "the wait did not settle");
        turn_once(&mut actor);
    }
    turn_once(&mut actor);
    assert_eq!(
        waits.get(run),
        None,
        "the Run no longer classifies as waiting"
    );
    assert!(!actor.orchestrator.run_publication_pending(run));
    assert_eq!(
        actor
            .orchestrator
            .ports_mut()
            .store
            .native_results()
            .unwrap()
            .native_result(&request.key)
            .unwrap()
            .unwrap()
            .stage,
        NativeResultStage::Registered
    );
    // The successor is an ordinary executing attempt.
    assert_eq!(
        ledger(&mut actor)
            .bot_execution(run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    let row = actor
        .orchestrator
        .ports_mut()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.attempt, row.generation, row.retired),
        (consumer.attempt, consumer.generation, false)
    );
    // The model's exact retry of the consume after settlement saves nothing.
    let replay = actor.bot_command(BotCommand::ConsumeDecisionEdit {
        fence: consumer,
        request,
        payload: binding.payload.clone(),
        proof: prepared.proof.clone(),
        boot: prepared.boot,
    });
    assert!(
        !matches!(&replay, Ok(BotReply::NativeResult { intent })
            if intent.disposition == NativeResultDisposition::Execute),
        "{replay:?}"
    );
}
