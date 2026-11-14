// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_core::*;
use koru_delegate_document::{RunDocument, RunDocumentError};
use koru_delegate_sqlite::{AttemptPhase, BotInitialSend};
use std::sync::atomic::AtomicBool;

struct PendingDocuments {
    snapshot: Vec<u8>,
    durable: Arc<AtomicBool>,
}
struct PendingHandle {
    doc: RunDocument,
    durable: Arc<AtomicBool>,
}
impl RunDocumentHandle for PendingHandle {
    fn document(&self) -> &RunDocument {
        &self.doc
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        write(&self.doc)?;
        Ok(())
    }
    fn durable(&self) -> bool {
        self.durable.load(Ordering::Acquire)
    }
    // The default settle returns without proving acknowledgment. The Bot owner
    // must still examine durable(), even when the adapter returns no error.
}
impl RunDocuments for PendingDocuments {
    fn create(&mut self, _: WorkspaceId, _: &RunHeader) -> PortResult<DocumentId> {
        unreachable!()
    }
    fn open(&mut self, _: WorkspaceId, _: DocumentId) -> PortResult<Box<dyn RunDocumentHandle>> {
        Ok(Box::new(PendingHandle {
            doc: RunDocument::from_snapshot(&self.snapshot)?,
            durable: self.durable.clone(),
        }))
    }
    fn list(&mut self, _: WorkspaceId) -> PortResult<Vec<koru_delegate_runtime::ListedRun>> {
        unreachable!()
    }
}

#[test]
fn delivery_command_returns_retryable_pending_then_applied_without_reopening_failed_run() {
    let (mut actor, _, _) = actor();
    let mut s = bot_submission();
    s.provenance.ai_attempt_id = Some(uuid::Uuid::new_v4());
    s.provenance.assistant_message_id = Some(uuid::Uuid::new_v4());
    actor.device = s.provenance.owner.device;
    let key = BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    };
    let store = &mut actor.orchestrator.ports_mut().store;
    store.bots().unwrap().admit_bot(&s).unwrap();
    store.satisfy_intent(key.run, DocumentId::new()).unwrap();
    store.bots().unwrap().reserve_bot(key.run, None).unwrap();
    let row = store.open_attempt(key.run).unwrap();
    store
        .advance(key.run, row.generation, AttemptPhase::Starting)
        .unwrap();
    store
        .bind_session(
            key.run,
            row.generation,
            ProviderSessionKey {
                idempotency: IdempotencyKey::mint(),
                session: Some(SessionId::mint()),
            },
            HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
        )
        .unwrap();
    store
        .advance(key.run, row.generation, AttemptPhase::Running)
        .unwrap();
    store
        .bots()
        .unwrap()
        .activate_bot(key.run, row.attempt, row.generation, None)
        .unwrap();
    let mut send = BotInitialSend {
        attempt: row.attempt,
        generation: row.generation,
        key: s.input.idempotency_key,
        fingerprint: [4; 32],
        turn: None,
    };
    store
        .bots()
        .unwrap()
        .prepare_bot_initial_send(key.run, &send, None)
        .unwrap();
    send.turn = Some(AgentTurnId::mint());
    store
        .bots()
        .unwrap()
        .acknowledge_bot_initial_send(key.run, &send)
        .unwrap();
    let receipt = BotDeliveryReceipt {
        run: key.run,
        attempt: row.attempt,
        generation: row.generation,
        chat_id: s.provenance.chat_id,
        ai_attempt_id: s.provenance.ai_attempt_id.unwrap(),
        assistant_message_id: s.provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: 1,
    };
    let mut header = RunHeader::from_request(s.request, 0);
    header.bot = Some(s.provenance);
    header.status = RunStatus::Cancelled;
    let durable = Arc::new(AtomicBool::new(false));
    actor.orchestrator.ports_mut().documents = Box::new(PendingDocuments {
        snapshot: RunDocument::create(&header)
            .unwrap()
            .export_snapshot()
            .unwrap(),
        durable: durable.clone(),
    });
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: BotCommand::AcknowledgeDelivery {
            key: key.clone(),
            receipt: receipt.clone(),
        },
        reply,
    });
    assert!(
        matches!(answer.recv().unwrap(), Response::Failure(failure) if failure.code == "bot_delivery_pending" && failure.retryable)
    );
    durable.store(true, Ordering::Release);
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: BotCommand::AcknowledgeDelivery {
            key: key.clone(),
            receipt: receipt.clone(),
        },
        reply,
    });
    assert!(matches!(
        answer.recv().unwrap(),
        Response::Bot(BotReply::Applied)
    ));
    assert_eq!(
        actor.orchestrator.header(key.run).unwrap().status,
        RunStatus::Cancelled
    );
    let mut wrong = receipt;
    wrong.confirmed_at_ms += 1;
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Bot {
        command: BotCommand::AcknowledgeDelivery {
            key,
            receipt: wrong,
        },
        reply,
    });
    assert!(
        matches!(answer.recv().unwrap(), Response::Failure(failure) if failure.code == "bot_conflict" && !failure.retryable)
    );
}

#[test]
fn queued_interrupt_reply_waits_for_canonical_ack_and_never_borrows_another_device() {
    let (mut actor, _, _) = actor();
    let s = bot_submission();
    actor.device = s.provenance.owner.device;
    let key = BotRunKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: s.request.run,
    };
    let store = &mut actor.orchestrator.ports_mut().store;
    store.bots().unwrap().admit_bot(&s).unwrap();
    store.satisfy_intent(key.run, DocumentId::new()).unwrap();
    let mut header = RunHeader::from_request(s.request, 0);
    header.bot = Some(s.provenance);
    assert_eq!(header.status, RunStatus::Created);
    let durable = Arc::new(AtomicBool::new(false));
    actor.orchestrator.ports_mut().documents = Box::new(PendingDocuments {
        snapshot: RunDocument::create(&header)
            .unwrap()
            .export_snapshot()
            .unwrap(),
        durable: durable.clone(),
    });
    let interrupt = |actor: &mut Actor, key: BotRunKey| {
        let (reply, answer) = mpsc::sync_channel(1);
        actor.handle(Command::Bot {
            command: BotCommand::Interrupt { key, fence: None },
            reply,
        });
        answer.recv().unwrap()
    };
    let mut foreign = key.clone();
    foreign.owner.device = DeviceId::from_uuid(uuid::Uuid::new_v4());
    assert!(
        matches!(interrupt(&mut actor, foreign), Response::Failure(failure) if failure.code == "bot_owner_mismatch" && !failure.retryable)
    );
    assert_eq!(
        actor.orchestrator.header(key.run).unwrap().status,
        RunStatus::Created
    );
    // The status is committed but not acknowledged: capacity stays queued and
    // the reply is the retryable canonical-durability failure, never Applied.
    assert!(
        matches!(interrupt(&mut actor, key.clone()), Response::Failure(failure) if failure.code == "bot_stop_unconfirmed" && failure.retryable)
    );
    let ledger = actor.orchestrator.ports_mut().store.bots().unwrap();
    assert_eq!(
        ledger.bot_execution(key.run).unwrap().unwrap().state,
        BotAdmissionState::Queued
    );
    durable.store(true, Ordering::Release);
    assert!(matches!(
        interrupt(&mut actor, key.clone()),
        Response::Bot(BotReply::Applied)
    ));
    assert!(matches!(
        interrupt(&mut actor, key.clone()),
        Response::Bot(BotReply::Applied)
    ));
    assert_eq!(
        actor.orchestrator.header(key.run).unwrap().status,
        RunStatus::Interrupted {
            prior_attempt: PriorAttempt::Unknown
        }
    );
    let ledger = actor.orchestrator.ports_mut().store.bots().unwrap();
    assert_eq!(
        ledger.bot_execution(key.run).unwrap().unwrap().state,
        BotAdmissionState::Settled
    );
    assert!(
        actor
            .orchestrator
            .ports_mut()
            .store
            .current_attempt(key.run)
            .unwrap()
            .is_none()
    );
}
