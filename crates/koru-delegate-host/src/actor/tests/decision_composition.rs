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
    loop {
        assert!(
            Instant::now() < deadline,
            "the chain did not reach the acknowledged successor: {:?}",
            actor.resume_log
        );
        actor.maintenance_tick();
        // Every worker wake (publication completion or enrollment reply) is
        // handled as the actor's loop handles it, which is also a turn.
        while let Ok(command) = inbox.recv_timeout(Duration::from_millis(20)) {
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
