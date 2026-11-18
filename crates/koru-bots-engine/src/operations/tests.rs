// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{
    Database,
    authority::{Call, Mint, Secret, Worker},
    messages,
};
use koru_delegate_core::{BotAccess, CapturedBotPolicy, NativeResultKind};
use koru_store_core::Timestamp;
use rusqlite::params;

struct Fixture {
    db: Database,
    worker: Worker,
    thread: Uuid,
    person: messages::Person,
    secret: Secret,
    call: Call,
    binding: Binding,
    prepared: Prepared,
    boundary: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let mut db = Database::in_memory().unwrap();
        let bot = Uuid::new_v4();
        let thread = Uuid::new_v4();
        let incarnation = Uuid::new_v4();
        let person = messages::Person {
            owner: Uuid::new_v4().to_string(),
            sender: "person".into(),
        };
        let workspace = WorkspaceId::new();
        let grant = CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Workspace,
            selected_pages: Default::default(),
            connections: Default::default(),
        };
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,?2,'fixture',x'7b7d',?3,x'5b5d','private')",params![bot.to_string(),person.owner,encode(&grant).unwrap()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,0,0)",params![bot.to_string(),incarnation.to_string()]).unwrap();
        db.accept_message(
            &person,
            &messages::Envelope {
                id: Uuid::new_v4(),
                to: bot,
                thread: Some(thread),
                in_reply_to: None,
                body: "write a page".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            },
            1,
        )
        .unwrap();
        let worker = Worker { bot, incarnation };
        let issued = db
            .mint_session(
                worker,
                Mint {
                    command: Uuid::new_v4(),
                    thread,
                    predecessor: None,
                },
                2,
            )
            .unwrap();
        let delivery = db
            .claim_session_messages(worker, issued.session, messages::Resume::Rebuilt, 65536, 3)
            .unwrap()
            .unwrap();
        db.delivery_accepted(delivery.turn, delivery.id).unwrap();
        let secret = issued.secret.unwrap();
        let call = Call::parse(
            Uuid::new_v4(),
            "create_document".into(),
            br#"{"title":"fixture"}"#,
        )
        .unwrap();
        let binding = db
            .bind_call(worker, &secret, &call, Uuid::new_v4(), 4)
            .unwrap();
        Self {
            db,
            worker,
            thread,
            person,
            secret,
            call,
            binding,
            boundary: Uuid::new_v4(),
            prepared: Prepared {
                kind: NativeEffectKind::CreateDocument,
                workspace,
                document: None,
                fingerprint: [42; 32],
                expected_revision: None,
                title: "fixture".into(),
            },
        }
    }
    fn record(&mut self) -> Record {
        self.db
            .record_operation(
                self.boundary,
                Uuid::new_v4(),
                &self.secret,
                self.binding.id,
                self.call.descriptor(),
                Uuid::new_v4(),
                self.prepared.clone(),
                5,
            )
            .unwrap()
    }
    fn acquire(&mut self, key: &Key, command: Uuid) -> Admission {
        self.db
            .acquire_operation(
                self.boundary,
                command,
                &self.secret,
                self.binding.id,
                self.call.descriptor(),
                key,
                self.prepared.fingerprint,
                6,
            )
            .unwrap()
    }
    fn leased(&mut self) -> Record {
        let record = self.record();
        let Admission::Execute(record) = self.acquire(&record.key, Uuid::new_v4()) else {
            panic!("one execution grant")
        };
        record
    }
    fn applied(record: &Record) -> Outcome {
        Outcome::Applied {
            evidence: NativeResultReference {
                operation_id: record.key.operation,
                workspace: record.store.workspace,
                document: record.store.document,
                version: vec![2, 1, 2, 2, 4],
                title: "fixture".into(),
                confirmed_at_ms: 7,
                kind: match record.store.kind {
                    NativeEffectKind::CreateDocument => NativeResultKind::CreatedPage,
                    NativeEffectKind::EditDocument => NativeResultKind::EditedPage,
                },
                version_id: None,
            },
            result: serde_json::json!({"document":record.store.document}),
        }
    }
    fn drain(&mut self, record: &Record) {
        self.db
            .record_operation_drain(
                &record.key,
                NativeEffectDrainReceipt {
                    binding: record.store.clone(),
                    fenced_at: Timestamp::from_millis(8),
                    drained_at: Timestamp::from_millis(9),
                },
            )
            .unwrap();
    }
}

#[test]
fn duplicate_intent_keeps_candidate_and_consumed_lease_never_replays_execute() {
    let mut f = Fixture::new();
    let record = f.record();
    let again =
        f.db.record_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            record.key.operation,
            f.prepared.clone(),
            5,
        )
        .unwrap();
    assert_eq!(record.store, again.store);
    assert_eq!(record.output, again.output);
    let command = Uuid::new_v4();
    assert!(matches!(
        f.acquire(&record.key, command),
        Admission::Execute(_)
    ));
    assert!(matches!(
        f.acquire(&record.key, command),
        Admission::Pending(_)
    ));
    assert!(matches!(
        f.acquire(&record.key, Uuid::new_v4()),
        Admission::Pending(_)
    ));
    let mut changed = f.prepared.clone();
    changed.title = "different".into();
    assert_eq!(
        f.db.record_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            record.key.operation,
            changed,
            6
        )
        .err(),
        Some(Error::OperationConflict)
    );
    f.db.connection
        .execute("UPDATE receipts SET answer=NULL", [])
        .unwrap();
    assert_eq!(
        f.db.acquire_operation(
            f.boundary,
            command,
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            &record.key,
            f.prepared.fingerprint,
            7
        )
        .err(),
        Some(Error::Expired)
    );
    assert!(matches!(
        f.acquire(&record.key, Uuid::new_v4()),
        Admission::Pending(_)
    ));
}

#[test]
fn lease_commit_failure_cannot_grant_execution() {
    let mut f = Fixture::new();
    let record = f.record();
    f.db.connection.execute_batch("CREATE TEMP TRIGGER reject_lease BEFORE UPDATE OF lease ON operations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.db.acquire_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            &record.key,
            f.prepared.fingerprint,
            6
        )
        .is_err()
    );
    assert_eq!(
        ledger::load(&f.db.connection, &record.key)
            .unwrap()
            .unwrap()
            .state,
        State::Recorded
    );
    f.db.connection
        .execute_batch("DROP TRIGGER reject_lease;")
        .unwrap();
    assert!(matches!(
        f.acquire(&record.key, Uuid::new_v4()),
        Admission::Execute(_)
    ));
}

#[test]
fn stop_revokes_acquire_but_valid_late_outcome_survives() {
    let mut f = Fixture::new();
    let record = f.leased();
    let pending = f.record();
    f.db.stop_turn(&f.person, Uuid::new_v4(), f.binding.turn, 7)
        .unwrap();
    assert_eq!(
        f.db.acquire_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            &pending.key,
            f.prepared.fingerprint,
            8
        )
        .err(),
        Some(Error::CredentialRevoked)
    );
    let applied =
        f.db.report_operation(
            f.boundary,
            Uuid::new_v4(),
            &record.key,
            record.lease.as_ref().unwrap(),
            Fixture::applied(&record),
            8,
        )
        .unwrap();
    assert_eq!(applied.state, State::Applied);
    assert_eq!(
        f.db.get_operation(&f.secret, f.binding.id, f.call.descriptor(), &record.key)
            .err(),
        Some(Error::CredentialRevoked)
    );
}

#[test]
fn outcome_checks_lease_intent_and_retains_conflicts_after_receipt_pruning() {
    let mut f = Fixture::new();
    let record = f.leased();
    let lease = record.lease.as_ref().unwrap();
    let outcome = Fixture::applied(&record);
    let mut wrong = lease.clone();
    wrong.binding.turn.generation += 1;
    assert_eq!(
        f.db.report_operation(
            f.boundary,
            Uuid::new_v4(),
            &record.key,
            &wrong,
            outcome.clone(),
            7
        )
        .err(),
        Some(Error::LeaseMismatch)
    );
    let mut wrong_outcome = outcome.clone();
    if let Outcome::Applied { evidence, .. } = &mut wrong_outcome {
        evidence.document = DocumentId::new();
    }
    assert_eq!(
        f.db.report_operation(
            f.boundary,
            Uuid::new_v4(),
            &record.key,
            lease,
            wrong_outcome,
            7
        )
        .err(),
        Some(Error::InvalidRequest)
    );
    f.db.report_operation(
        f.boundary,
        Uuid::new_v4(),
        &record.key,
        lease,
        outcome.clone(),
        7,
    )
    .unwrap();
    f.db.report_operation(f.boundary, Uuid::new_v4(), &record.key, lease, outcome, 8)
        .unwrap();
    let refused = Outcome::Refused {
        stable_code: "not_allowed".into(),
    };
    assert_eq!(
        f.db.report_operation(f.boundary, Uuid::new_v4(), &record.key, lease, refused, 9)
            .err(),
        Some(Error::OutcomeConflict)
    );
    f.db.connection
        .execute("UPDATE receipts SET answer=NULL", [])
        .unwrap();
    let retained = ledger::load(&f.db.connection, &record.key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.state, State::Applied);
    assert_eq!(retained.recovery.observations.len(), 1);
    assert!(matches!(
        f.acquire(&record.key, Uuid::new_v4()),
        Admission::Terminal(_)
    ));
}

#[test]
fn execute_not_received_requires_original_command_and_same_boundary_boot() {
    let mut f = Fixture::new();
    let record = f.leased();
    let lease = record.lease.as_ref().unwrap();
    assert_eq!(
        f.db.execute_not_received(
            Uuid::new_v4(),
            Uuid::new_v4(),
            &record.key,
            lease.acquire_command,
            7
        )
        .err(),
        Some(Error::LeaseMismatch)
    );
    assert_eq!(
        f.db.execute_not_received(f.boundary, Uuid::new_v4(), &record.key, Uuid::new_v4(), 7)
            .err(),
        Some(Error::LeaseMismatch)
    );
    assert_eq!(
        f.db.execute_not_received(
            f.boundary,
            Uuid::new_v4(),
            &record.key,
            lease.acquire_command,
            7
        )
        .unwrap()
        .state,
        State::Refused
    );
    assert!(matches!(
        f.acquire(&record.key, Uuid::new_v4()),
        Admission::Terminal(_)
    ));
}

#[test]
fn reconciliation_requires_exact_drain_and_continue_never_releases_old_key() {
    let mut f = Fixture::new();
    let record = f.leased();
    assert_eq!(
        f.db.reconcile_operation(&record.key, None).err(),
        Some(Error::RecoveryPending)
    );
    let mut wrong = record.store.clone();
    wrong.request_key = Uuid::new_v4();
    assert_eq!(
        f.db.record_operation_drain(
            &record.key,
            NativeEffectDrainReceipt {
                binding: wrong,
                fenced_at: Timestamp::from_millis(8),
                drained_at: Timestamp::from_millis(9)
            }
        )
        .err(),
        Some(Error::InvalidRequest)
    );
    assert_eq!(
        f.db.continue_operations(
            &f.person,
            Uuid::new_v4(),
            f.thread,
            &[record.key.clone()],
            10
        )
        .err(),
        Some(Error::RecoveryPending)
    );
    f.drain(&record);
    assert_eq!(
        f.db.reconcile_operation(&record.key, None).unwrap().state,
        State::Unresolved
    );
    assert!(
        f.db.claim_messages(
            f.worker.bot,
            f.thread,
            f.worker.incarnation,
            Uuid::new_v4(),
            messages::Resume::Rebuilt,
            65536,
            11
        )
        .unwrap()
        .is_none()
    );
    // Late valid observations remain supplemental. Continue includes them.
    assert_eq!(
        f.db.report_operation(
            f.boundary,
            Uuid::new_v4(),
            &record.key,
            record.lease.as_ref().unwrap(),
            Fixture::applied(&record),
            11
        )
        .err(),
        Some(Error::OutcomeConflict)
    );
    let command = Uuid::new_v4();
    let continued =
        f.db.continue_operations(&f.person, command, f.thread, &[record.key.clone()], 12)
            .unwrap();
    assert!(!continued.replayed);
    assert!(
        f.db.continue_operations(&f.person, command, f.thread, &[record.key.clone()], 12)
            .unwrap()
            .replayed
    );
    let old = ledger::load(&f.db.connection, &record.key)
        .unwrap()
        .unwrap();
    assert_eq!(old.state, State::Unresolved);
    assert!(old.recovery.drain.is_some());
    let next =
        f.db.claim_messages(
            f.worker.bot,
            f.thread,
            f.worker.incarnation,
            Uuid::new_v4(),
            messages::Resume::Rebuilt,
            65536,
            13,
        )
        .unwrap()
        .unwrap();
    let instructions = next
        .work
        .iter()
        .find(|m| m.id == continued.answer.messages[0])
        .unwrap();
    assert!(
        instructions
            .content
            .body
            .contains("Read each referenced page")
    );
    assert_eq!(
        instructions.content.references[0]["observations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(old.lease.unwrap().id, record.lease.unwrap().id);
}

#[test]
fn current_and_captured_grants_both_restrict_writes_and_retained_results() {
    let mut f = Fixture::new();
    f.prepared.kind = NativeEffectKind::EditDocument;
    f.prepared.document = Some(DocumentId::new());
    f.prepared.expected_revision = Some(vec![2, 1, 2, 2, 4]);
    f.call = Call::parse(
        Uuid::new_v4(),
        "edit_document".into(),
        br#"{"document":"fixture"}"#,
    )
    .unwrap();
    f.binding =
        f.db.bind_call(f.worker, &f.secret, &f.call, Uuid::new_v4(), 4)
            .unwrap();
    let record = f.leased();
    f.db.report_operation(
        f.boundary,
        Uuid::new_v4(),
        &record.key,
        record.lease.as_ref().unwrap(),
        Fixture::applied(&record),
        7,
    )
    .unwrap();
    f.db.connection.execute("UPDATE bots SET access=CAST(json_set(CAST(access AS TEXT),'$.access','default') AS BLOB)",[]).unwrap();
    assert_eq!(
        f.db.get_operation(&f.secret, f.binding.id, f.call.descriptor(), &record.key)
            .err(),
        Some(Error::NotAllowed)
    );
    f.db.connection.execute("UPDATE bots SET access=CAST(json_set(CAST(access AS TEXT),'$.access','workspace') AS BLOB)",[]).unwrap();
    f.db.connection.execute("UPDATE turns SET grant=CAST(json_set(CAST(grant AS TEXT),'$.access','default') AS BLOB)",[]).unwrap();
    assert_eq!(
        f.db.get_operation(&f.secret, f.binding.id, f.call.descriptor(), &record.key)
            .err(),
        Some(Error::NotAllowed)
    );
}

#[test]
fn expected_revision_uses_native_logical_equality_not_encoded_order() {
    let mut f = Fixture::new();
    f.prepared.kind = NativeEffectKind::EditDocument;
    f.prepared.document = Some(DocumentId::new());
    f.prepared.expected_revision = Some(vec![2, 1, 2, 2, 4]);
    f.call = Call::parse(Uuid::new_v4(), "edit_document".into(), b"{}").unwrap();
    f.binding =
        f.db.bind_call(f.worker, &f.secret, &f.call, Uuid::new_v4(), 4)
            .unwrap();
    let record = f.record();
    let mut equal = f.prepared.clone();
    equal.expected_revision = Some(vec![2, 2, 4, 1, 2]);
    assert!(
        f.db.record_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            record.key.operation,
            equal,
            6
        )
        .is_ok()
    );
    f.prepared.expected_revision = Some(vec![255]);
    assert_eq!(
        f.db.record_operation(
            f.boundary,
            Uuid::new_v4(),
            &f.secret,
            f.binding.id,
            f.call.descriptor(),
            record.key.operation,
            f.prepared.clone(),
            6
        )
        .err(),
        Some(Error::InvalidRequest)
    );
}

#[test]
fn output_is_derived_from_evidence_and_late_result_attaches_to_exact_reply() {
    for report_first in [true, false] {
        let mut f = Fixture::new();
        let record = f.leased();
        let report = |f: &mut Fixture| {
            f.db.report_operation(
                f.boundary,
                Uuid::new_v4(),
                &record.key,
                record.lease.as_ref().unwrap(),
                Fixture::applied(&record),
                8,
            )
            .unwrap()
        };
        if report_first {
            report(&mut f);
        }
        let published =
            f.db.publish_reply(
                &messages::Publish {
                    command: Uuid::new_v4(),
                    turn: f.binding.turn,
                    body: "finished".into(),
                    outputs: vec![],
                },
                9,
            )
            .unwrap()
            .answer;
        if !report_first {
            report(&mut f);
        }
        let (message, id, reference): (String, String, Vec<u8>) =
            f.db.connection
                .query_row("SELECT message,id,reference FROM outputs", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap();
        assert_eq!(message, published.message.to_string());
        assert_eq!(id, record.output.to_string());
        let evidence: NativeResultReference = decode(&reference).unwrap();
        assert_eq!(evidence.document, record.store.document);
        // An identical report cannot attach a duplicate output.
        report(&mut f);
        assert_eq!(
            f.db.connection
                .query_row("SELECT count(*) FROM outputs", [], |r| r.get::<_, usize>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn intent_recorded_in_first_turn_but_executed_in_second_belongs_to_second_reply() {
    let mut f = Fixture::new();
    let record = f.record();
    let first = f.binding.turn;
    f.db.interrupt_turn(first).unwrap();
    let issued =
        f.db.mint_session(
            f.worker,
            Mint {
                command: Uuid::new_v4(),
                thread: f.thread,
                predecessor: Some(first.session),
            },
            7,
        )
        .unwrap();
    let delivery =
        f.db.claim_session_messages(
            f.worker,
            issued.session,
            messages::Resume::Rebuilt,
            65536,
            8,
        )
        .unwrap()
        .unwrap();
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    f.secret = issued.secret.unwrap();
    f.call = Call::parse(
        Uuid::new_v4(),
        "create_document".into(),
        br#"{"title":"fixture"}"#,
    )
    .unwrap();
    f.binding =
        f.db.bind_call(f.worker, &f.secret, &f.call, Uuid::new_v4(), 8)
            .unwrap();
    let Admission::Execute(record) = f.acquire(&record.key, Uuid::new_v4()) else {
        panic!("first lease")
    };
    assert_eq!(record.recorded_call.turn, first);
    assert_eq!(record.lease.as_ref().unwrap().binding.turn, delivery.turn);
    f.db.report_operation(
        f.boundary,
        Uuid::new_v4(),
        &record.key,
        record.lease.as_ref().unwrap(),
        Fixture::applied(&record),
        9,
    )
    .unwrap();
    assert!(
        outputs::collect(&f.db.connection, first)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        outputs::collect(&f.db.connection, delivery.turn)
            .unwrap()
            .len(),
        1
    );
    let published =
        f.db.publish_reply(
            &messages::Publish {
                command: Uuid::new_v4(),
                turn: delivery.turn,
                body: "done".into(),
                outputs: vec![],
            },
            10,
        )
        .unwrap()
        .answer;
    assert_eq!(
        f.db.connection
            .query_row("SELECT message FROM outputs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        published.message.to_string()
    );
}

#[test]
fn continue_rejects_a_stale_set_and_a_foreign_person_without_consuming_work() {
    let mut f = Fixture::new();
    let first = f.leased();
    let second = f.leased();
    f.drain(&first);
    f.drain(&second);
    f.db.reconcile_operation(&first.key, None).unwrap();
    f.db.reconcile_operation(&second.key, None).unwrap();
    assert_eq!(
        f.db.continue_operations(
            &f.person,
            Uuid::new_v4(),
            f.thread,
            &[first.key.clone()],
            10
        )
        .err(),
        Some(Error::RecoveryPending)
    );
    let stranger = messages::Person {
        owner: f.person.owner.clone(),
        sender: "stranger".into(),
    };
    assert_eq!(
        f.db.continue_operations(
            &stranger,
            Uuid::new_v4(),
            f.thread,
            &[first.key.clone(), second.key.clone()],
            10
        )
        .err(),
        Some(Error::NotAllowed)
    );
    assert!(
        ledger::load(&f.db.connection, &first.key)
            .unwrap()
            .unwrap()
            .recovery
            .continued_by
            .is_none()
    );
    let continued =
        f.db.continue_operations(
            &f.person,
            Uuid::new_v4(),
            f.thread,
            &[second.key, first.key],
            11,
        )
        .unwrap();
    assert_eq!(continued.answer.messages.len(), 1);
}

#[test]
fn outputs_have_a_composite_operation_foreign_key() {
    let mut f = Fixture::new();
    let record = f.leased();
    f.db.report_operation(
        f.boundary,
        Uuid::new_v4(),
        &record.key,
        record.lease.as_ref().unwrap(),
        Fixture::applied(&record),
        8,
    )
    .unwrap();
    let message =
        f.db.publish_reply(
            &messages::Publish {
                command: Uuid::new_v4(),
                turn: f.binding.turn,
                body: "done".into(),
                outputs: vec![],
            },
            9,
        )
        .unwrap()
        .answer
        .message;
    assert!(f.db.connection.execute("INSERT INTO outputs(id,message,operation,scope,bot,thread,kind,reference) VALUES (?1,?2,?3,'foreign',?4,?5,'page',x'7b7d')",params![Uuid::new_v4().to_string(),message.to_string(),record.key.operation.to_string(),record.key.bot.to_string(),record.key.thread.to_string()]).is_err());
    assert!(
        f.db.connection
            .execute(
                "DELETE FROM operations WHERE id=?1",
                [record.key.operation.to_string()]
            )
            .is_err()
    );
}
