// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_api::bots::{BotCommand, BotReply};
use koru_delegate_core::{NativeResultKey, ProviderSessionKey};
use koru_delegate_document::{RunDocument, RunDocumentError};
use koru_delegate_sqlite::{AttemptPhase, BotInitialSend, BotLedger};

struct ReadOnlyDocument(RunDocument);
impl RunDocumentHandle for ReadOnlyDocument {
    fn document(&self) -> &RunDocument {
        &self.0
    }
    fn commit(
        &mut self,
        _: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        panic!("an absent native operation must not mutate its Run")
    }
}
struct OneDocument(RunHeader);
impl RunDocuments for OneDocument {
    fn create(&mut self, _: WorkspaceId, _: &RunHeader) -> PortResult<DocumentId> {
        unreachable!()
    }
    fn open(&mut self, _: WorkspaceId, _: DocumentId) -> PortResult<Box<dyn RunDocumentHandle>> {
        Ok(Box::new(ReadOnlyDocument(
            RunDocument::create(&self.0).unwrap(),
        )))
    }
    fn list(&mut self, _: WorkspaceId) -> PortResult<Vec<koru_delegate_runtime::ListedRun>> {
        unreachable!()
    }
}

#[test]
fn native_reconcile_returns_missing_only_for_the_validated_durable_execution() {
    let (mut actor, _, _) = actor();
    let submission = bot_submission();
    actor.device = submission.provenance.owner.device;
    let run = submission.request.run;
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    db.admit_bot(&submission).unwrap();
    db.satisfy_intent(run, DocumentId::new()).unwrap();
    db.reserve_bot(run, None).unwrap();
    let attempt = db.open_attempt(run).unwrap();
    db.advance(run, attempt.generation, AttemptPhase::Starting)
        .unwrap();
    let host = HostInstanceId::from_uuid(uuid::Uuid::new_v4());
    let idempotency = IdempotencyKey::mint();
    db.bind_session(
        run,
        attempt.generation,
        ProviderSessionKey {
            idempotency,
            session: None,
        },
        host,
    )
    .unwrap();
    db.bind_session(
        run,
        attempt.generation,
        ProviderSessionKey {
            idempotency,
            session: Some(SessionId::mint()),
        },
        host,
    )
    .unwrap();
    db.advance(run, attempt.generation, AttemptPhase::Running)
        .unwrap();
    db.activate_bot(run, attempt.attempt, attempt.generation, None)
        .unwrap();
    let mut send = BotInitialSend {
        attempt: attempt.attempt,
        generation: attempt.generation,
        key: IdempotencyKey::mint(),
        fingerprint: [1; 32],
        turn: None,
    };
    db.prepare_bot_initial_send(run, &send, None).unwrap();
    let turn = AgentTurnId::mint();
    send.turn = Some(turn);
    db.acknowledge_bot_initial_send(run, &send).unwrap();
    let mut header = RunHeader::from_request(submission.request.clone(), 0);
    header.bot = Some(submission.provenance);
    actor.orchestrator.ports_mut().store = Box::new(db);
    actor.orchestrator.ports_mut().documents = Box::new(OneDocument(header));
    let key = NativeResultKey {
        run,
        attempt: attempt.attempt,
        generation: attempt.generation,
        turn,
        operation_id: uuid::Uuid::new_v4(),
    };
    assert!(matches!(
        actor
            .bot_command(BotCommand::ReconcileNativeResult { key: key.clone() })
            .unwrap(),
        BotReply::Missing
    ));
    for altered in [
        NativeResultKey {
            turn: AgentTurnId::mint(),
            ..key.clone()
        },
        NativeResultKey {
            generation: key.generation.next(),
            ..key.clone()
        },
        NativeResultKey {
            run: RunId::mint(),
            ..key.clone()
        },
        NativeResultKey {
            attempt: koru_delegate_core::RunAttemptId::mint(),
            ..key.clone()
        },
    ] {
        assert!(
            actor
                .bot_command(BotCommand::ReconcileNativeResult { key: altered })
                .is_err()
        );
    }
    actor.device = DeviceId::new();
    assert!(
        matches!(actor.bot_command(BotCommand::ReconcileNativeResult { key }), Err(error) if error.code == "bot_owner_mismatch")
    );
}
