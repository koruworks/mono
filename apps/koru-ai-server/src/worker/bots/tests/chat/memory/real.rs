// SPDX-License-Identifier: MPL-2.0
//! Actual Memory history on the shared ContextJob, with a synthetic runtime.
use super::*;
use crate::worker::bots::memory::composition::{self, Fixture, gate::Gate};

#[test]
fn stop_during_real_memory_context_read_processes_late_callback_without_begin() {
    let (fixture, repository) = Fixture::new();
    let authority = Arc::new(Gate::new(fixture.authority.clone()));
    let native = Arc::new(Inputs {
        native: Native::default(),
        inputs: Default::default(),
        refuse_submit: AtomicBool::new(false),
    });
    let (completed, completion) = sync_channel(4);
    let running = fixture.start_observed(
        repository,
        authority.clone(),
        native.clone(),
        Some(completed),
    );
    let (_, page) = fixture.ensure(&running);
    fixture.facts(&page, "retained");
    let (entered, release) = authority.hold_replay();
    let stream = send(&running.worker, request(&fixture.bot));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(native.native.state.lock().unwrap().submission.is_none());
    let (reply, response) = sync_channel(1);
    running
        .worker
        .client()
        .send(WorkerCommand::CancelAttempt {
            context: composition::context().into(),
            attempt: stream.accepted.attempt_id,
            reply,
        })
        .unwrap();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Cancelled
    );
    release.send(()).unwrap();
    assert_eq!(
        completion.recv_timeout(Duration::from_secs(10)).unwrap(),
        stream.accepted.attempt_id
    );
    // The Context envelope was enqueued before the report. This Ping, sent
    // only after observing it, proves the actual late callback was processed.
    running.worker.ping().unwrap();
    let state = native.native.state.lock().unwrap();
    assert_eq!(state.begin, 0);
    assert!(
        state.submission.is_none(),
        "context preparation must not submit native work"
    );
    assert_eq!(state.stop_calls, 0);
    drop(state);
    assert_eq!(authority.replay_calls.load(Ordering::SeqCst), 1);
    running.stop();
}

#[test]
fn restart_during_real_memory_context_read_interrupts_without_resend() {
    let (fixture, repository) = Fixture::new();
    let authority = Arc::new(Gate::new(fixture.authority.clone()));
    let native = Arc::new(Inputs {
        native: Native::default(),
        inputs: Default::default(),
        refuse_submit: AtomicBool::new(false),
    });
    let running = fixture.start_ports(repository, authority.clone(), native.clone());
    let (_, page) = fixture.ensure(&running);
    fixture.facts(&page, "retained");
    let (entered, release) = authority.hold_replay();
    let stream = send(&running.worker, request(&fixture.bot));
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    running.worker.shutdown();
    release.send(()).unwrap();
    let repository = running.stop();
    let receipt = repository
        .bot_submission_for_attempt(stream.accepted.attempt_id)
        .unwrap()
        .unwrap();
    let capture = repository
        .bot_library_capture(
            fixture.authority.owner().unwrap(),
            fixture.bot.id,
            receipt.intent.submission.provenance.submission,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(capture.memory, BotCapturedMemory::Captured { .. }));
    let submission = receipt.intent.submission.provenance.submission;
    let restarted = fixture.start_ports(repository, authority.clone(), native.clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (reply, response) = sync_channel(1);
        restarted
            .worker
            .client()
            .send(WorkerCommand::ReadAttempt {
                context: composition::context().into(),
                attempt: stream.accepted.attempt_id,
                reply,
            })
            .unwrap();
        match response.recv_timeout(Duration::from_secs(10)).unwrap() {
            Ok(response) => {
                assert_eq!(response.attempt.status, AttemptStatus::Interrupted);
                break;
            }
            Err(error) if error.code == "revision_conflict" => {
                // Startup admission reconciliation can change the canonical
                // Bot while this normal read's authority is being prepared.
                assert!(Instant::now() < deadline, "{error:?}");
            }
            Err(error) => panic!("unexpected read failure: {error:?}"),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match fixture.owner(
            &restarted,
            BotCommand::ReadOperation {
                bot_id: fixture.bot.id,
                request_id: submission,
            },
        ) {
            Ok(BotResponse::Operation { operation }) => {
                if operation.status == BotOperationStatus::Rejected {
                    break;
                }
                assert_eq!(operation.status, BotOperationStatus::Pending);
            }
            Err(error) if error.code == "revision_conflict" => {}
            other => panic!("unexpected recovery receipt: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "admission recovery did not settle"
        );
    }
    // Rejected is durable proof that the asynchronous recovery completed its
    // fence-and-reconcile chain. A Ping alone could precede that chain.
    restarted.worker.ping().unwrap();
    let state = native.native.state.lock().unwrap();
    assert_eq!(state.begin, 0);
    assert!(state.submission.is_none());
    assert_eq!(state.interrupt_calls, 0);
    assert_eq!(state.stop_calls, 0);
    drop(state);
    assert_eq!(authority.replay_calls.load(Ordering::SeqCst), 1);
    restarted.stop();
}
