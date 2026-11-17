// SPDX-License-Identifier: MPL-2.0

//! Piece R through the real actor (M6, contract 10.3). Scripted composition,
//! not provider qualification. The runtime's child-chain builder hands this
//! actor an orchestrator whose parent Bot Run is executing its first turn;
//! the ask arrives on the private Bot route (`RequestChild`), and the actor
//! then drives the wait with its REAL maintenance tick (the per-turn child
//! driver, the real fixed publication worker and its child enrollment
//! mailbox, the clock deadline). Enrollment is answered by the host's own
//! `HostDecisionBridge` on the child route over a loopback HTTP fixture
//! (127.0.0.1, no disk). The session host is the builder's script. No
//! Store, provider, browser or disk.

use super::decision_composition::accept_within;
use super::*;
use crate::actor::bots::bot_failure;
use crate::actor::child_resume::ChildTurn;
use crate::decision_bridge::tests::{body, bridge};
use koru_delegate_api::bots::{BotCommand, BotReply, ChildRequestedOutcome};
use koru_delegate_api::decision_bridge::DecisionBridgeEnvelope;
use koru_delegate_core::children::{
    BotChildWaitEnrollmentReply, ChildRefusal, ChildRequestState, ChildSettledBy,
    EnrollBotChildWaitCleanup,
};
use koru_delegate_core::decisions::BotWaitPhase;
use koru_delegate_runtime::ChildResumeStep as Step;
use koru_delegate_runtime::test_support::{ChildChain, child_chain};
use std::io::Write as _;
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;

fn child_steps(log: &[ChildTurn]) -> Vec<Step> {
    log.iter()
        .filter_map(|turn| match turn {
            ChildTurn::Stepped(_, step) => Some(*step),
            _ => None,
        })
        .collect()
}

/// Answer one child enrollment request on the loopback fixture.
fn child_reply(
    stream: &mut std::net::TcpStream,
    payload: impl FnOnce(&EnrollBotChildWaitCleanup) -> BotChildWaitEnrollmentReply,
) {
    let received =
        DecisionBridgeEnvelope::<EnrollBotChildWaitCleanup>::decode(&body(stream)).unwrap();
    let answer = DecisionBridgeEnvelope::reply_to(&received, payload(&received.payload))
        .unwrap()
        .encode()
        .unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        answer.len()
    )
    .unwrap();
    stream.write_all(&answer).unwrap();
}

#[test]
fn m6_child_actor_the_thirteen_steps_on_the_real_tick() {
    let ChildChain {
        orchestrator,
        sessions,
        parent,
        ..
    } = child_chain();
    let (owner, run) = (parent.owner, parent.run);
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    parent.script_resume(&mut sessions.lock().unwrap(), successor, turn);

    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    // The decision bridge starts the fixed worker; the child client rides it.
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&unused)))
        .unwrap();
    actor
        .attach_child_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&requests);
    let ai_attempt = parent.ai_attempt;
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(30));
        child_reply(&mut stream, |request| {
            served.fetch_add(1, Ordering::SeqCst);
            BotChildWaitEnrollmentReply::Enrolled {
                receipt_digest: request.key.receipt_digest,
                key: request.key.clone(),
                ai_attempt,
            }
        });
    });

    // 1-3: the ask, on the private Bot route.
    let request = parent.ask(&mut actor.orchestrator, &[1]);
    let reply = actor.bot_command(BotCommand::RequestChild {
        request: request.clone(),
        boot: parent.boot,
    });
    let Ok(BotReply::ChildRequested {
        outcome,
        wait,
        receipts,
        ..
    }) = reply
    else {
        panic!("the ask was not admitted: {reply:?}");
    };
    assert_eq!(outcome, ChildRequestedOutcome::Created);
    assert_eq!(wait.phase, BotWaitPhase::HoldRequested);
    // The same frame again replays.
    let again = actor.bot_command(BotCommand::RequestChild {
        request: request.clone(),
        boot: parent.boot,
    });
    assert!(
        matches!(
            &again,
            Ok(BotReply::ChildRequested {
                outcome: ChildRequestedOutcome::AlreadyCreated,
                ..
            })
        ),
        "{again:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut child_stopped = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "the child wait did not resume: steps {:?}",
            child_steps(&actor.child_log)
        );
        actor.maintenance_tick();
        let drain_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < drain_until {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            actor.handle(command);
        }
        let taken = child_steps(&actor.child_log);
        if !child_stopped && taken.contains(&Step::OriginStopped) {
            // The host slot is free (the origin is retired); the Bot's
            // admission is still Executing.
            let row = actor
                .orchestrator
                .ports()
                .store
                .current_attempt(run)
                .unwrap()
                .unwrap();
            assert!(row.retired, "the parent's host slot is free while it waits");
            let record = actor
                .orchestrator
                .ports_mut()
                .store
                .bots()
                .unwrap()
                .bot_execution(run)
                .unwrap()
                .unwrap();
            assert_eq!(
                record.state,
                koru_delegate_core::BotAdmissionState::Executing
            );
            // 7-8: a person stops the queued child, on the private route.
            let stopped = actor.bot_command(BotCommand::Stop {
                key: koru_delegate_core::BotRunKey {
                    owner,
                    bot: receipts[0].recipient,
                    run: receipts[0].child_run,
                },
                fence: None,
            });
            assert!(stopped.is_ok(), "{stopped:?}");
            child_stopped = true;
        }
        if taken.last() == Some(&Step::InputAcknowledged) {
            break;
        }
    }
    server.join().unwrap();
    assert_eq!(
        child_steps(&actor.child_log),
        vec![
            Step::Enrolled,
            Step::OriginStopped,
            Step::SuccessorReserved,
            Step::SuccessorBound,
            Step::SuccessorInstalled,
            Step::InputAcknowledged,
        ]
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1, "one enrollment request");
    // The request settled by the child's own end, read on the private route.
    let read = actor.bot_command(BotCommand::ReadChildRequests {
        subject: koru_delegate_core::BotRunKey {
            owner,
            bot: parent.bot,
            run,
        },
    });
    let Ok(BotReply::ChildRequests { requests: read, .. }) = read else {
        panic!("{read:?}");
    };
    assert_eq!(read[0].state, ChildRequestState::Settled);
    assert_eq!(read[0].settled_by, Some(ChildSettledBy::ChildEnded));
    // The successor: created once, attached.
    let script = sessions.lock().unwrap();
    assert!(script.attached(successor));
    assert_eq!(script.created().len(), 2);
}

/// Cohort `m6_origin_stop`: the thirteen steps again, on a session host that
/// behaves as the real one does. The real session adapter serves the
/// decision-window stream, and a session Koru terminates reports its own end
/// on it (`SessionEvent::Exited`). The scripted host of the test above
/// refuses that stream, so nothing there ever saw the asking session end.
///
/// Koru's own stop of the asking session, for an enrolled wait, is not the
/// asking Run ending: the request stays open, the wait is not cleaned up,
/// and the Run is resumed by a successor.
#[test]
fn m6_origin_stop_the_stopped_sessions_own_end_does_not_cancel_the_request() {
    let ChildChain {
        orchestrator,
        sessions,
        parent,
        ..
    } = child_chain();
    let (owner, run) = (parent.owner, parent.run);
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    {
        let mut script = sessions.lock().unwrap();
        script.report_session_ends();
        parent.script_resume(&mut script, successor, turn);
    }

    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&unused)))
        .unwrap();
    actor
        .attach_child_bridge(Box::new(bridge(&listener)))
        .unwrap();
    let ai_attempt = parent.ai_attempt;
    let server = std::thread::spawn(move || {
        let mut stream = accept_within(&listener, Duration::from_secs(30));
        child_reply(&mut stream, |request| {
            BotChildWaitEnrollmentReply::Enrolled {
                receipt_digest: request.key.receipt_digest,
                key: request.key.clone(),
                ai_attempt,
            }
        });
    });

    let request = parent.ask(&mut actor.orchestrator, &[1]);
    let reply = actor.bot_command(BotCommand::RequestChild {
        request,
        boot: parent.boot,
    });
    let Ok(BotReply::ChildRequested { receipts, .. }) = reply else {
        panic!("the ask was not admitted: {reply:?}");
    };
    let subject = koru_delegate_core::BotRunKey {
        owner,
        bot: parent.bot,
        run,
    };
    let read = |actor: &mut Actor| {
        let read = actor.bot_command(BotCommand::ReadChildRequests {
            subject: subject.clone(),
        });
        let Ok(BotReply::ChildRequests { wait, requests }) = read else {
            panic!("{read:?}");
        };
        (wait, requests)
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut child_stopped = false;
    // Ticks after the origin stop during which the request must stay open:
    // long enough for the stopped session's end to be read and acted on.
    let mut held = 0;
    loop {
        assert!(
            Instant::now() < deadline,
            "the child wait did not resume: steps {:?}",
            child_steps(&actor.child_log)
        );
        actor.maintenance_tick();
        let drain_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < drain_until {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            actor.handle(command);
        }
        // The actor loop's budget deadline, which is where a retained
        // cleanup of a Run is requested (`publication_cleanup_tick`).
        for due in actor.orchestrator.bot_budget_runs() {
            let _ = actor.orchestrator.bot_budget_tick(due);
        }
        let taken = child_steps(&actor.child_log);
        let (wait, requests) = read(&mut actor);
        // Whatever the stopped session reported, the asking Run did not end.
        assert_ne!(
            requests[0].settled_by,
            Some(ChildSettledBy::ParentStopped),
            "Koru's own stop of the asking session canceled the request: steps {taken:?}, wait {:?}",
            wait.as_ref().map(|wait| wait.phase)
        );
        if !child_stopped && taken.contains(&Step::OriginStopped) {
            assert_eq!(requests[0].state, ChildRequestState::Open);
            assert_eq!(
                wait.as_ref().map(|wait| wait.phase),
                Some(BotWaitPhase::Retired),
                "the wait holds for the asked Bot; it is not cleaned up"
            );
            held += 1;
            if held < 4 {
                continue;
            }
            // The asked Bot's Run ends (a person stops the queued child).
            let stopped = actor.bot_command(BotCommand::Stop {
                key: koru_delegate_core::BotRunKey {
                    owner,
                    bot: receipts[0].recipient,
                    run: receipts[0].child_run,
                },
                fence: None,
            });
            assert!(stopped.is_ok(), "{stopped:?}");
            child_stopped = true;
        }
        if taken.last() == Some(&Step::InputAcknowledged) {
            break;
        }
    }
    server.join().unwrap();
    assert_eq!(
        child_steps(&actor.child_log),
        vec![
            Step::Enrolled,
            Step::OriginStopped,
            Step::SuccessorReserved,
            Step::SuccessorBound,
            Step::SuccessorInstalled,
            Step::InputAcknowledged,
        ]
    );
    let (_, requests) = read(&mut actor);
    assert_eq!(requests[0].state, ChildRequestState::Settled);
    assert_eq!(requests[0].settled_by, Some(ChildSettledBy::ChildEnded));
    // The stopped session did report its end, and the successor was created.
    let script = sessions.lock().unwrap();
    assert!(
        script.calls().iter().any(|call| matches!(
            call,
            koru_delegate_runtime::test_support::SessionCall::Subscribe(_)
        )),
        "the decision-window stream was served"
    );
    assert!(script.attached(successor));
    assert_eq!(script.created().len(), 2);
}

/// Cohort `m6_origin_stop`, the other side: the asking session ends **by
/// itself** while the wait is not enrolled (the AI worker never answers).
/// Nobody authorized a stop, so this is the asking Run ending: the request
/// is settled `parent_stopped` and the wait goes to its cleanup owner, as
/// before the origin stop was told apart.
#[test]
fn m6_origin_stop_an_asking_session_that_ends_by_itself_still_cancels_the_request() {
    let ChildChain {
        orchestrator,
        sessions,
        parent,
        ..
    } = child_chain();
    let (owner, run) = (parent.owner, parent.run);
    sessions.lock().unwrap().report_session_ends();
    let (commands, inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, owner.device.to_storage());
    // The bridges exist and nobody answers: the wait is never enrolled.
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    actor
        .attach_decision_bridge(Box::new(bridge(&unused)))
        .unwrap();
    actor
        .attach_child_bridge(Box::new(bridge(&silent)))
        .unwrap();
    let request = parent.ask(&mut actor.orchestrator, &[1]);
    let reply = actor.bot_command(BotCommand::RequestChild {
        request,
        boot: parent.boot,
    });
    assert!(
        matches!(reply, Ok(BotReply::ChildRequested { .. })),
        "{reply:?}"
    );
    let subject = koru_delegate_core::BotRunKey {
        owner,
        bot: parent.bot,
        run,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut turns = 0;
    let settled = loop {
        assert!(
            Instant::now() < deadline,
            "the request of a Run that ended was not settled: steps {:?}",
            child_steps(&actor.child_log)
        );
        actor.maintenance_tick();
        let drain_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < drain_until {
            let Ok(command) = inbox.recv_timeout(Duration::from_millis(5)) else {
                break;
            };
            actor.handle(command);
        }
        for due in actor.orchestrator.bot_budget_runs() {
            let _ = actor.orchestrator.bot_budget_tick(due);
        }
        turns += 1;
        if turns == 3 {
            // The asking session's process ends on its own.
            sessions.lock().unwrap().end_session(parent.fence.session);
        }
        let read = actor.bot_command(BotCommand::ReadChildRequests {
            subject: subject.clone(),
        });
        let Ok(BotReply::ChildRequests { wait, requests }) = read else {
            panic!("{read:?}");
        };
        if requests[0].state == ChildRequestState::Settled {
            break (wait, requests);
        }
        assert!(turns >= 3 || requests[0].state == ChildRequestState::Open);
    };
    assert!(turns >= 3, "nothing was settled before the session ended");
    assert!(!child_steps(&actor.child_log).contains(&Step::Enrolled));
    assert_eq!(settled.1[0].settled_by, Some(ChildSettledBy::ParentStopped));
    assert_eq!(
        settled.0.map(|wait| wait.phase),
        Some(BotWaitPhase::CleanupPending)
    );
}

/// Cohort `m6_child_codes_final`, at the actor's failure mapping: every
/// `bot_child_*` refusal reaches the wire final, except
/// `bot_child_wait_closing`, and a refused ask answers its code.
#[test]
fn m6_child_codes_final_through_bot_failure() {
    let refusals = [
        ChildRefusal::DecisionOpen,
        ChildRefusal::WaitOpen,
        ChildRefusal::WaitClosing,
        ChildRefusal::Depth,
        ChildRefusal::SelfAsk,
        ChildRefusal::RecipientLimit,
        ChildRefusal::Cycle,
        ChildRefusal::RecipientPaused,
        ChildRefusal::RecipientStale,
        ChildRefusal::RecipientFull,
        ChildRefusal::Capacity,
        ChildRefusal::BudgetLow,
        ChildRefusal::ConnectionsNotSubset,
        ChildRefusal::FolderOverlap,
    ];
    for refusal in refusals {
        let failure = bot_failure(koru_delegate_runtime::RunError::Port(
            koru_delegate_runtime::PortError::new(
                koru_delegate_runtime::Port::Store,
                refusal.code(),
                "detail that must not cross",
            ),
        ));
        assert_eq!(failure.code, refusal.code());
        assert!(failure.code.starts_with("bot_child_"));
        assert_eq!(
            failure.retryable,
            refusal == ChildRefusal::WaitClosing,
            "{}",
            failure.code
        );
        assert!(!failure.message.contains("detail that must not cross"));
    }
    let unknown = bot_failure(koru_delegate_runtime::RunError::Port(
        koru_delegate_runtime::PortError::new(
            koru_delegate_runtime::Port::Store,
            koru_delegate_api::bots::CHILD_WAIT_UNKNOWN,
            "x",
        ),
    ));
    assert!(!unknown.retryable);
    // A scope that is still being recovered is not final.
    let busy = bot_failure(koru_delegate_runtime::RunError::Port(
        koru_delegate_runtime::PortError::new(
            koru_delegate_runtime::Port::Scope,
            "scope_recovery_pending",
            "x",
        ),
    ));
    assert!(busy.retryable);
}

/// A refused ask through the actor commits nothing and answers its code.
#[test]
fn m6_child_codes_final_a_refused_ask_answers_its_code_on_the_route() {
    let ChildChain {
        mut orchestrator,
        parent,
        ..
    } = child_chain();
    let parent_scope = parent.submission.request.scope.clone();
    let request = parent.ask_with(&mut orchestrator, &[1], |submission| {
        submission.request.scope = parent_scope.clone();
    });
    let (commands, _inbox) = mpsc::channel();
    let mut actor = Actor::new(orchestrator, commands, parent.owner.device.to_storage());
    let refused = actor
        .bot_command(BotCommand::RequestChild {
            request,
            boot: parent.boot,
        })
        .unwrap_err();
    assert_eq!(refused.code, "bot_child_folder_overlap");
    assert!(!refused.retryable);
}
