// SPDX-License-Identifier: MPL-2.0

//! Real fixed worker and Loro staging, fake acknowledgement transport only.
//! No Store connection, provider, disk or default Run5 exposure is involved.

use super::*;
use koru_delegate_core::decisions::DecisionTransitionId;
use koru_delegate_document::{RunDocument, RunDocumentError, RunProjectionStaging};
use koru_delegate_runtime::RunProjectionIntent;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Handle {
    document: RunDocument,
    staging: RunProjectionStaging,
    applies: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl RunDocumentHandle for Handle {
    fn document(&self) -> &RunDocument {
        &self.document
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        write(&self.document)?;
        Ok(())
    }
    fn prepare_decision_projection(
        &mut self,
        intent: &RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection> {
        Ok(self.staging.prepare(&self.document, intent)?)
    }
    fn apply_prepared_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<AppliedRunProjection> {
        self.applies.fetch_add(1, Ordering::SeqCst);
        self.staging.apply(&self.document, prepared)?;
        Ok(self
            .staging
            .acknowledge(&self.document, prepared, &self.document.version())?)
    }
    fn discard_prepared_projection(&mut self, transition: DecisionTransitionId) -> PortResult<()> {
        Ok(self.staging.discard(transition)?)
    }
}

fn fixture() -> (RunPublicationStart, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (document, intent) = crate::documents::projection_fixture();
    let applies = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let ticket = RunPublicationTicket {
        run: intent.identity.run,
        epoch: uuid::Uuid::new_v4(),
    };
    (
        RunPublicationStart {
            ticket,
            document: RunPublicationDocument::Attached(Box::new(Handle {
                document,
                staging: RunProjectionStaging::default(),
                applies: applies.clone(),
                dropped: dropped.clone(),
            })),
            operation: RunPublicationOperation::Prepare(intent),
        },
        applies,
        dropped,
    )
}

fn completion(worker: &mut PublicationWorker, wake: &Receiver<()>) -> Completion {
    wake.recv_timeout(Duration::from_secs(5))
        .expect("bounded worker wake");
    worker.completion().expect("completion precedes wake")
}

fn shutdown_bounded(mut worker: PublicationWorker) -> Vec<Completion> {
    let (send, receive) = mpsc::sync_channel(1);
    let joined = thread::spawn(move || {
        let _ = send.send(worker.shutdown());
    });
    let result = receive
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown must drain the completion slot");
    joined.join().unwrap();
    result
}

#[test]
fn publication_worker_holds_one_handle_until_exact_writer_receipt() {
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    let (start, applies, dropped) = fixture();
    let ticket = start.ticket;
    assert!(worker.begin(start).is_ok());
    let Completion::Prepared {
        prepared,
        ticket: returned,
    } = completion(&mut worker, &wake)
    else {
        panic!("prepared")
    };
    assert_eq!(returned, ticket);
    assert_eq!(
        applies.load(Ordering::SeqCst),
        0,
        "preparation never imports without writer receipt"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let (other, _, _) = fixture();
    assert!(
        worker.begin(other).is_err(),
        "no second workflow while Prepared retains its handle"
    );
    worker.apply(ticket, prepared.clone()).unwrap();
    let Completion::Applied { proof, handle, .. } = completion(&mut worker, &wake) else {
        panic!("applied")
    };
    assert_eq!(proof.identity(), &prepared.identity);
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    assert!(worker.ticket().is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(handle);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(shutdown_bounded(worker).is_empty());
}

#[test]
fn publication_worker_shutdown_with_queued_request_returns_handle() {
    let (send, requests) = mpsc::sync_channel(1);
    let (completed, receive) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        gate.recv().unwrap();
        run(requests, completed, || {});
    });
    let mut worker = PublicationWorker {
        send,
        receive,
        thread: Some(thread),
        ticket: None,
        waiting: false,
        enrollment: EnrollmentSlot::new(mpsc::sync_channel(1).1),
    };
    let (start, applies, dropped) = fixture();
    let ticket = start.ticket;
    assert!(worker.begin(start).is_ok());
    assert!(
        worker.waiting,
        "the request is queued before the worker starts"
    );
    release.send(()).unwrap();
    let completions = shutdown_bounded(worker);
    assert_eq!(completions.len(), 2);
    assert!(matches!(&completions[0], Completion::Prepared { ticket: t, .. } if *t == ticket));
    assert!(matches!(&completions[1], Completion::Returned { ticket: t, .. } if *t == ticket));
    assert_eq!(applies.load(Ordering::SeqCst), 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(completions);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn publication_worker_shutdown_drains_undelivered_completion_before_return() {
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    let (start, applies, dropped) = fixture();
    assert!(worker.begin(start).is_ok());
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    // Prepared physically occupies the capacity-one completion slot. The actor
    // deliberately has not consumed it when shutdown queues its control.
    let completions = shutdown_bounded(worker);
    assert_eq!(completions.len(), 2);
    assert!(matches!(completions[0], Completion::Prepared { .. }));
    assert!(matches!(completions[1], Completion::Returned { .. }));
    assert_eq!(applies.load(Ordering::SeqCst), 0);
    drop(completions);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn publication_worker_cold_open_runs_only_on_reserved_worker_thread() {
    struct Open {
        handle: Box<dyn RunDocumentHandle>,
        caller: thread::ThreadId,
        opens: Arc<AtomicUsize>,
    }
    impl koru_delegate_runtime::RunDocumentOpen for Open {
        fn open(self: Box<Self>) -> PortResult<Box<dyn RunDocumentHandle>> {
            assert_ne!(thread::current().id(), self.caller);
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(self.handle)
        }
    }
    let (mut start, applies, dropped) = fixture();
    let RunPublicationDocument::Attached(handle) = start.document else {
        unreachable!()
    };
    let opens = Arc::new(AtomicUsize::new(0));
    start.document = RunPublicationDocument::Cold(Box::new(Open {
        handle,
        caller: thread::current().id(),
        opens: opens.clone(),
    }));
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "specification construction does not open"
    );
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    assert!(worker.begin(start).is_ok());
    let Completion::Prepared { ticket, .. } = completion(&mut worker, &wake) else {
        panic!("prepared")
    };
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(applies.load(Ordering::SeqCst), 0);
    worker.return_handle(ticket).unwrap();
    let Completion::Returned { handle, .. } = completion(&mut worker, &wake) else {
        panic!("returned")
    };
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(handle);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(shutdown_bounded(worker).is_empty());
}

#[test]
fn publication_worker_shutdown_records_actual_loss_when_thread_ends_without_reply() {
    let (send, requests) = mpsc::sync_channel(1);
    let (completed, receive) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        let Request::Start(start) = requests.recv().unwrap() else {
            panic!("start")
        };
        // Models an actual ended worker stack, not a logical timeout. Its sole
        // handle is dropped before the completion endpoint disappears.
        drop(start);
        drop(completed);
    });
    let mut worker = PublicationWorker {
        send,
        receive,
        thread: Some(thread),
        ticket: None,
        waiting: false,
        enrollment: EnrollmentSlot::new(mpsc::sync_channel(1).1),
    };
    let (start, applies, dropped) = fixture();
    let ticket = start.ticket;
    assert!(worker.begin(start).is_ok());
    let completions = shutdown_bounded(worker);
    assert_eq!(completions.len(), 1);
    assert!(matches!(completions[0], Completion::Lost { ticket: actual } if actual == ticket));
    assert_eq!(applies.load(Ordering::SeqCst), 0);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "physical handle ended before Loss"
    );
}

#[test]
fn publication_worker_unknown_apply_retains_same_handle_and_exact_bytes_for_replay() {
    struct Uncertain {
        inner: Box<dyn RunDocumentHandle>,
        first: bool,
    }
    impl RunDocumentHandle for Uncertain {
        fn document(&self) -> &RunDocument {
            self.inner.document()
        }
        fn commit(
            &mut self,
            write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
        ) -> PortResult<()> {
            self.inner.commit(write)
        }
        fn prepare_decision_projection(
            &mut self,
            intent: &RunProjectionIntent,
        ) -> PortResult<DecisionPreparedProjection> {
            self.inner.prepare_decision_projection(intent)
        }
        fn apply_prepared_projection(
            &mut self,
            prepared: &DecisionPreparedProjection,
        ) -> PortResult<AppliedRunProjection> {
            let applied = self.inner.apply_prepared_projection(prepared)?;
            if std::mem::take(&mut self.first) {
                // Real import already applied; only the owning reply is lost.
                return Err(PortError::new(
                    Port::Documents,
                    "fixture_unknown",
                    "prepared apply reply is unknown",
                ));
            }
            Ok(applied)
        }
        fn discard_prepared_projection(
            &mut self,
            transition: DecisionTransitionId,
        ) -> PortResult<()> {
            self.inner.discard_prepared_projection(transition)
        }
    }
    let (mut start, applies, dropped) = fixture();
    let RunPublicationDocument::Attached(handle) = start.document else {
        unreachable!()
    };
    start.document = RunPublicationDocument::Attached(Box::new(Uncertain {
        inner: handle,
        first: true,
    }));
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    assert!(worker.begin(start).is_ok());
    let Completion::Prepared { ticket, prepared } = completion(&mut worker, &wake) else {
        panic!("prepared")
    };
    worker.apply(ticket, prepared.clone()).unwrap();
    assert!(
        matches!(completion(&mut worker, &wake), Completion::Pending { ticket: current, .. } if current == ticket)
    );
    assert_eq!(worker.ticket(), Some(ticket));
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    let (other, _, _) = fixture();
    assert!(
        worker.begin(other).is_err(),
        "unknown completion cannot free the physical slot"
    );
    worker.return_handle(ticket).unwrap();
    let Completion::Returned { handle, .. } = completion(&mut worker, &wake) else {
        panic!("returned")
    };
    let timeline = handle.document().timeline_len();
    let retry_ticket = RunPublicationTicket {
        run: ticket.run,
        epoch: uuid::Uuid::new_v4(),
    };
    assert!(
        worker
            .begin(RunPublicationStart {
                ticket: retry_ticket,
                document: RunPublicationDocument::Attached(handle),
                operation: RunPublicationOperation::Apply(prepared.clone()),
            })
            .is_ok()
    );
    let Completion::Applied {
        ticket: returned,
        proof,
        handle,
    } = completion(&mut worker, &wake)
    else {
        panic!("applied")
    };
    assert_eq!(returned, retry_ticket);
    assert_eq!(proof.identity(), &prepared.identity);
    assert_eq!(
        handle.document().timeline_len(),
        timeline,
        "same bytes do not append again"
    );
    assert_eq!(applies.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(handle);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(shutdown_bounded(worker).is_empty());
}

// ---------------------------------------------------- the enrollment job

fn v4<T: std::str::FromStr>() -> T
where
    T::Err: std::fmt::Debug,
{
    uuid::Uuid::new_v4().to_string().parse().unwrap()
}

fn enrollment_request() -> EnrollBotWaitCleanup {
    use koru_delegate_core::decisions::*;
    let owner: koru_delegate_core::BotOwner = serde_json::from_value(serde_json::json!({
        "user_scope": uuid::Uuid::new_v4().to_string(),
        "device": uuid::Uuid::new_v4().to_string(),
    }))
    .unwrap();
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner,
        bot: v4(),
        run: v4(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(2).unwrap(),
        wait_revision: DecisionRevision::new(3).unwrap(),
        origin_attempt: v4(),
        origin_generation: koru_delegate_core::AttemptGeneration::from_raw(1).unwrap(),
        origin_session: v4(),
        origin_turn: v4(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    EnrollBotWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    }
}

fn enrolled(request: &EnrollBotWaitCleanup) -> BotWaitEnrollmentReply {
    BotWaitEnrollmentReply::Enrolled {
        receipt_digest: request.key.receipt_digest,
        key: request.key.clone(),
        // Deterministic per request, so two replies for one key compare equal.
        ai_attempt: request.key.wait_id.as_uuid(),
    }
}

/// Scripted host client: answers each call from its queue (an empty queue is
/// a lost reply), optionally waiting on a gate first. Counts physical calls.
struct Client {
    calls: Arc<AtomicUsize>,
    replies: std::collections::VecDeque<Option<BotWaitEnrollmentReply>>,
    gate: Option<Receiver<()>>,
}
impl DecisionEnrollmentBridge for Client {
    fn enroll(&mut self, _request: &EnrollBotWaitCleanup) -> PortResult<BotWaitEnrollmentReply> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let _ = gate.recv();
        }
        self.replies
            .pop_front()
            .flatten()
            .ok_or_else(|| PortError::new(Port::Documents, "unavailable", "reply lost"))
    }
}

fn pending(result: PortResult<BotWaitEnrollmentReply>) -> bool {
    matches!(result, Err(error) if error.code == "decision_enrollment_pending")
}

#[test]
fn enrollment_never_runs_while_a_document_handle_is_out() {
    let (send, wake) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let request = enrollment_request();
    let mut worker = PublicationWorker::start_with_enrollment(
        move || {
            let _ = send.send(());
        },
        Box::new(Client {
            calls: calls.clone(),
            replies: [Some(enrolled(&request))].into(),
            gate: None,
        }),
    )
    .unwrap();
    let (start, _, _) = fixture();
    let ticket = start.ticket;
    assert!(worker.begin(start).is_ok());
    let Completion::Prepared { .. } = completion(&mut worker, &wake) else {
        panic!("prepared")
    };
    // Prepared retains the handle: enrollment waits and nothing is sent.
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    worker.return_handle(ticket).unwrap();
    let Completion::Returned { handle, .. } = completion(&mut worker, &wake) else {
        panic!("returned")
    };
    drop(handle);
    // With the handle back the job starts; its reply is claimed on a later turn.
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    let reply = EnrollmentMailbox::new(&mut worker)
        .enroll(&request)
        .unwrap();
    assert_eq!(reply, enrolled(&request));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(shutdown_bounded(worker).is_empty());
}

#[test]
fn enrollment_lost_reply_stays_unknown_and_the_same_request_is_retried() {
    let (send, wake) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let request = enrollment_request();
    let mut worker = PublicationWorker::start_with_enrollment(
        move || {
            let _ = send.send(());
        },
        Box::new(Client {
            calls: calls.clone(),
            replies: [None, Some(enrolled(&request))].into(),
            gate: None,
        }),
    )
    .unwrap();
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    let lost = EnrollmentMailbox::new(&mut worker).enroll(&request);
    assert!(
        matches!(&lost, Err(error) if error.code == "unavailable"),
        "a lost reply is an error, never a reply: {lost:?}"
    );
    // The next turn sends the same request again.
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        EnrollmentMailbox::new(&mut worker)
            .enroll(&request)
            .unwrap(),
        enrolled(&request)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(shutdown_bounded(worker).is_empty());
}

#[test]
fn enrollment_reply_for_another_key_is_never_returned_and_is_discarded() {
    let (send, wake) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let mine = enrollment_request();
    let other = enrollment_request();
    let mut worker = PublicationWorker::start_with_enrollment(
        move || {
            let _ = send.send(());
        },
        Box::new(Client {
            calls: calls.clone(),
            replies: [Some(enrolled(&mine)), Some(enrolled(&mine))].into(),
            gate: None,
        }),
    )
    .unwrap();
    assert!(pending(EnrollmentMailbox::new(&mut worker).enroll(&mine)));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    // Another key never receives this reply; on its second attempt the
    // unclaimed reply is discarded and that key's own job starts.
    assert!(pending(EnrollmentMailbox::new(&mut worker).enroll(&other)));
    assert!(pending(EnrollmentMailbox::new(&mut worker).enroll(&other)));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // The discarded reply is gone: the original key must ask again.
    let late = EnrollmentMailbox::new(&mut worker).enroll(&mine);
    assert!(pending(late), "never handed a reply meant for another job");
    assert!(shutdown_bounded(worker).is_empty());
}

#[test]
fn shutdown_during_an_in_flight_enrollment_joins_after_the_physical_call() {
    let (gate_send, gate) = mpsc::sync_channel(1);
    let calls = Arc::new(AtomicUsize::new(0));
    let request = enrollment_request();
    let mut worker = PublicationWorker::start_with_enrollment(
        || {},
        Box::new(Client {
            calls: calls.clone(),
            replies: [Some(enrolled(&request))].into(),
            gate: Some(gate),
        }),
    )
    .unwrap();
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    // A caller giving up frees nothing: the slot stays occupied.
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    let (start, _, dropped) = fixture();
    assert!(worker.begin(start).is_err(), "the single ticket is held");
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "the refused handle came back"
    );
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        gate_send.send(()).unwrap();
    });
    assert!(shutdown_bounded(worker).is_empty());
    release.join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn enrollment_without_a_client_is_unavailable_never_a_reply() {
    let (send, wake) = mpsc::channel();
    let mut worker = PublicationWorker::start(move || {
        let _ = send.send(());
    })
    .unwrap();
    let request = enrollment_request();
    assert!(pending(
        EnrollmentMailbox::new(&mut worker).enroll(&request)
    ));
    wake.recv_timeout(Duration::from_secs(5)).unwrap();
    let answer = EnrollmentMailbox::new(&mut worker).enroll(&request);
    assert!(matches!(&answer, Err(error) if error.code == "decision_enrollment_unavailable"));
    assert!(shutdown_bounded(worker).is_empty());
}
