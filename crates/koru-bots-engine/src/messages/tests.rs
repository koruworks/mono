// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::params;
mod input_budget;
mod scripted;

struct Fixture {
    db: Database,
    person: Person,
    bot: Uuid,
    thread: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let db = Database::in_memory().unwrap();
        let bot = Uuid::new_v4();
        let thread = Uuid::new_v4();
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",params![thread.to_string(),bot.to_string()]).unwrap();
        Self {
            db,
            person: Person {
                sender: "person".into(),
                owner: "owner".into(),
            },
            bot,
            thread,
        }
    }
    fn envelope(&self, body: &str) -> Envelope {
        Envelope {
            id: Uuid::new_v4(),
            to: self.bot,
            thread: Some(self.thread),
            in_reply_to: None,
            body: body.into(),
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        }
    }
    fn send(&mut self, body: &str) -> Accepted {
        self.db
            .accept_message(&self.person, &self.envelope(body), 1)
            .unwrap()
            .answer
    }
    fn claim(&mut self, resume: Resume) -> Delivery {
        self.db
            .claim_messages(
                self.bot,
                self.thread,
                Uuid::new_v4(),
                Uuid::new_v4(),
                resume,
                64 * 1024,
                2,
            )
            .unwrap()
            .unwrap()
    }
    fn count(&self, table: &str) -> u64 {
        self.db
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn state(&self, obligation_message: Uuid) -> String {
        self.db
            .connection
            .query_row(
                "SELECT state FROM obligations WHERE message=?1",
                [obligation_message.to_string()],
                |r| r.get(0),
            )
            .unwrap()
    }
}
fn reply(delivery: &Delivery) -> Publish {
    Publish {
        command: Uuid::new_v4(),
        turn: delivery.turn,
        body: "fixture reply".into(),
        outputs: vec![],
    }
}

#[test]
fn acceptance_is_atomic_and_retry_and_tombstone_never_duplicate_work() {
    let mut f = Fixture::new();
    let envelope = f.envelope("one");
    f.db.connection.execute_batch("CREATE TEMP TRIGGER reject_inbox BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.db.accept_message(&f.person, &envelope, 1).is_err());
    assert_eq!(f.count("messages"), 0);
    assert_eq!(f.count("receipts"), 0);
    f.db.connection
        .execute_batch("DROP TRIGGER reject_inbox;")
        .unwrap();
    let first = f.db.accept_message(&f.person, &envelope, 1).unwrap();
    let again = f.db.accept_message(&f.person, &envelope, 2).unwrap();
    assert!(!first.replayed && again.replayed);
    assert_eq!(first.answer, again.answer);
    assert_eq!(f.count("inbox"), 1);
    let mut changed = envelope.clone();
    changed.body = "different".into();
    assert_eq!(
        f.db.accept_message(&f.person, &changed, 2).err(),
        Some(Error::Conflict)
    );
    f.db.prune_receipt_answers(RETENTION_MS + 1).unwrap();
    assert_eq!(
        f.db.accept_message(&f.person, &envelope, RETENTION_MS + 2)
            .err(),
        Some(Error::Expired)
    );
    assert_eq!(
        f.db.accept_message(&f.person, &changed, RETENTION_MS + 2)
            .err(),
        Some(Error::Conflict)
    );
    changed.id = Uuid::new_v4();
    assert!(
        f.db.accept_message(&f.person, &changed, RETENTION_MS + 2)
            .is_ok()
    );
    assert_eq!(f.count("inbox"), 2);
}

#[test]
fn sixty_five_owed_messages_are_three_deliveries_and_never_context() {
    let mut f = Fixture::new();
    for _ in 0..65 {
        f.send("work");
    }
    for expected in [32, 32, 1] {
        let delivery = f.claim(Resume::Rebuilt);
        assert_eq!(delivery.work.len(), expected);
        for message in &delivery.context {
            let owed:bool=f.db.connection.query_row("SELECT EXISTS(SELECT 1 FROM obligations WHERE message=?1 AND state IN ('owed','delivered'))",[message.id.to_string()],|r|r.get(0)).unwrap();
            assert!(!owed, "undelivered requests are not context");
        }
        f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
        let result = f.db.publish_reply(&reply(&delivery), 3).unwrap();
        assert_eq!(result.answer.answered, expected as u64);
    }
    assert_eq!(f.count("inbox"), 0);
    assert_eq!(f.count("obligations"), 65);
}

#[test]
fn stop_after_offer_cancels_only_its_batch_and_rejects_late_worker_results() {
    let mut f = Fixture::new();
    for _ in 0..65 {
        f.send("work");
    }
    let delivery = f.claim(Resume::Rebuilt);
    let command = Uuid::new_v4();
    assert_eq!(
        f.db.stop_turn(&f.person, command, delivery.turn, 3)
            .unwrap()
            .answer
            .cancelled,
        32
    );
    assert!(
        f.db.stop_turn(&f.person, command, delivery.turn, 4)
            .unwrap()
            .replayed
    );
    assert_eq!(f.count("inbox"), 33);
    assert_eq!(
        f.db.delivery_accepted(delivery.turn, delivery.id),
        Err(Error::StaleTurn)
    );
    assert_eq!(
        f.db.publish_reply(&reply(&delivery), 4).err(),
        Some(Error::StaleTurn)
    );
    assert_eq!(f.claim(Resume::Rebuilt).work.len(), 32);
}

#[test]
fn crash_on_either_side_of_acceptance_keeps_work_for_rebuilt_session() {
    for acknowledged in [false, true] {
        let mut f = Fixture::new();
        let message = f.send("work");
        let old = f.claim(Resume::Rebuilt);
        if acknowledged {
            f.db.delivery_accepted(old.turn, old.id).unwrap();
        }
        if acknowledged {
            f.db.recover_interrupted_turns().unwrap();
        } else {
            f.db.interrupt_worker(old.turn.worker).unwrap();
        }
        assert_eq!(f.state(message.message), "delivered");
        let new = f.claim(Resume::Rebuilt);
        assert!(new.continuation);
        assert_eq!(new.work.len(), 1);
        assert!(new.turn.generation > old.turn.generation);
        assert_eq!(
            f.db.stop_turn(&f.person, Uuid::new_v4(), old.turn, 3).err(),
            Some(Error::StaleTurn)
        );
        assert_eq!(
            f.db.publish_reply(&reply(&old), 3).err(),
            Some(Error::StaleTurn)
        );
        f.db.delivery_accepted(new.turn, new.id).unwrap();
        f.db.publish_reply(&reply(&new), 4).unwrap();
        assert_eq!(f.state(message.message), "answered");
    }
}

#[test]
fn resumed_history_proof_carries_obligation_without_resending_content() {
    for found in [false, true] {
        let mut f = Fixture::new();
        let message = f.send("work");
        let old = f.claim(Resume::Rebuilt);
        f.db.interrupt_worker(old.turn.worker).unwrap();
        let new = f.claim(Resume::Resumed {
            found_deliveries: if found { vec![old.id] } else { vec![] },
        });
        assert_eq!(new.work.len(), if found { 0 } else { 1 });
        assert_eq!(new.uncertain, !found);
        assert_eq!(
            new.resume_accepted,
            if found { vec![old.id] } else { vec![] }
        );
        f.db.delivery_accepted(new.turn, new.id).unwrap();
        f.db.publish_reply(&reply(&new), 4).unwrap();
        assert_eq!(f.state(message.message), "answered");
    }
}

#[test]
fn publication_does_not_answer_unknown_delivery_even_after_late_confirmation() {
    let mut f = Fixture::new();
    let message = f.send("work");
    let old = f.claim(Resume::Rebuilt);
    assert_eq!(
        f.db.publish_reply(&reply(&old), 3).unwrap().answer.answered,
        0
    );
    f.db.delivery_accepted(old.turn, old.id).unwrap();
    assert_eq!(f.state(message.message), "delivered");
    let new = f.claim(Resume::Rebuilt);
    assert!(new.continuation && new.uncertain);
    assert_eq!(new.work.len(), 1);
}

#[test]
fn expiry_only_removes_owed_work_and_preserves_accepted_running_work() {
    let mut f = Fixture::new();
    let mut envelope = f.envelope("work");
    envelope.deadline_ms = Some(5);
    let message = f.db.accept_message(&f.person, &envelope, 1).unwrap().answer;
    let delivery = f.claim(Resume::Rebuilt);
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let mut waiting = f.envelope("still waiting");
    waiting.deadline_ms = Some(5);
    let waiting = f.db.accept_message(&f.person, &waiting, 1).unwrap().answer;
    assert_eq!(f.db.expire_messages(6).unwrap(), 1);
    assert_eq!(f.state(waiting.message), "expired");
    assert_eq!(
        f.db.publish_reply(&reply(&delivery), 6)
            .unwrap()
            .answer
            .answered,
        1
    );
    assert_eq!(f.state(message.message), "answered");
    assert_eq!(f.count("messages"), 3);
    assert_eq!(f.count("inbox"), 0);
}

#[test]
fn consecutive_resume_proves_original_delivery_and_reconciles_missing_history() {
    let mut f = Fixture::new();
    f.send("original work");
    let old = f.claim(Resume::Rebuilt);
    f.db.interrupt_worker(old.turn.worker).unwrap();
    let resumed = f.claim(Resume::Resumed {
        found_deliveries: vec![old.id],
    });
    assert!(resumed.work.is_empty());
    let read: u64 =
        f.db.connection
            .query_row(
                "SELECT read_sequence FROM participants WHERE participant=?1",
                [f.bot.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        read, old.history_boundary,
        "replay proves the old read point before new input is accepted"
    );
    f.db.interrupt_worker(resumed.turn.worker).unwrap();
    // Only the original input reached the provider; the carried record's id
    // is engine-local and cannot be present in provider replay.
    let twice = f.claim(Resume::Resumed {
        found_deliveries: vec![old.id],
    });
    assert!(twice.work.is_empty());
    assert_eq!(twice.resume_accepted, vec![old.id]);
    f.db.interrupt_worker(twice.turn.worker).unwrap();
    let missing = f.claim(Resume::Resumed {
        found_deliveries: vec![],
    });
    assert_eq!(missing.work.len(), 1);
    let state: String =
        f.db.connection
            .query_row(
                "SELECT state FROM deliveries WHERE id=?1",
                [old.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(state, "unknown");
    let read: u64 =
        f.db.connection
            .query_row(
                "SELECT read_sequence FROM participants WHERE participant=?1",
                [f.bot.to_string()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        read, old.history_boundary,
        "absence never advances the read point"
    );
}

#[test]
fn output_must_belong_to_an_applied_call_of_this_turn() {
    let mut f = Fixture::new();
    f.send("first work");
    let first = f.claim(Resume::Rebuilt);
    f.db.delivery_accepted(first.turn, first.id).unwrap();
    let operation = Uuid::new_v4();
    let binding = Uuid::new_v4();
    f.db.connection.execute("INSERT INTO tool_calls(binding,door_call,session,turn,generation,tool,argument_digest,details) VALUES (?1,?1,?2,?3,?4,'page',zeroblob(32),x'7b7d')",params![binding.to_string(),first.turn.session.to_string(),first.turn.id.to_string(),first.turn.generation]).unwrap();
    f.db.connection.execute("INSERT INTO operations(scope,bot,thread,id,argument_digest,recorded_call,intent,state) VALUES ('owner',?1,?2,?3,zeroblob(32),?4,x'7b7d','applied')",params![f.bot.to_string(),f.thread.to_string(),operation.to_string(),binding.to_string()]).unwrap();
    let output = Output {
        id: Uuid::new_v4(),
        operation,
        kind: OutputKind::Page,
        reference: serde_json::json!({"page":"fixture"}),
    };
    let mut publication = reply(&first);
    publication.outputs.push(output.clone());
    assert_eq!(
        f.db.publish_reply(&publication, 3).unwrap().answer.answered,
        1
    );
    f.send("second work");
    let second = f.claim(Resume::Rebuilt);
    f.db.delivery_accepted(second.turn, second.id).unwrap();
    let mut publication = reply(&second);
    publication.outputs.push(Output {
        id: Uuid::new_v4(),
        ..output
    });
    let before = f.count("messages");
    assert_eq!(
        f.db.publish_reply(&publication, 4).err(),
        Some(Error::InvalidRequest)
    );
    assert_eq!(f.count("messages"), before);
    assert_eq!(f.db.status().unwrap().running_turns, 1);
}

#[test]
fn delivery_budget_cuts_unicode_without_changing_the_stored_message() {
    let mut f = Fixture::new();
    let body = "👋\"".repeat(5000);
    f.send(&body);
    assert_eq!(
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            16,
            2
        )
        .err(),
        Some(Error::InputTooSmall)
    );
    assert_eq!(f.count("turns"), 0);
    let delivery =
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            2048,
            2,
        )
        .unwrap()
        .unwrap();
    assert!(delivery.work[0].truncated);
    assert!(encode(&delivery).unwrap().len() <= 2048);
    assert_eq!(
        f.db.read_messages(&f.person, f.thread, 0).unwrap()[0]
            .content
            .body,
        body
    );
}

#[test]
fn two_threads_run_independently_and_reference_is_committed_with_reply() {
    let mut f = Fixture::new();
    f.send("main work");
    let main = f.claim(Resume::Rebuilt);
    let mut envelope = f.envelope("side work");
    envelope.thread = None;
    let side = f.db.accept_message(&f.person, &envelope, 1).unwrap().answer;
    let delivery =
        f.db.claim_messages(
            f.bot,
            side.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            64 * 1024,
            2,
        )
        .unwrap()
        .unwrap();
    assert_eq!(f.db.status().unwrap().running_turns, 2);
    f.db.stop_turn(&f.person, Uuid::new_v4(), main.turn, 3)
        .unwrap();
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let publish = reply(&delivery);
    f.db.connection.execute_batch("CREATE TEMP TRIGGER reject_card BEFORE INSERT ON messages WHEN NEW.kind='thread_reference' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.db.publish_reply(&publish, 4).is_err());
    assert_eq!(f.count("messages"), 2);
    assert_eq!(f.db.status().unwrap().running_turns, 1);
    f.db.connection
        .execute_batch("DROP TRIGGER reject_card;")
        .unwrap();
    assert!(!f.db.publish_reply(&publish, 4).unwrap().replayed);
    assert!(f.db.publish_reply(&publish, 5).unwrap().replayed);
    let cards: u64 =
        f.db.connection
            .query_row(
                "SELECT count(*) FROM messages WHERE kind='thread_reference'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(cards, 1);
}

#[test]
fn output_without_applied_evidence_rolls_back_the_entire_publication() {
    let mut f = Fixture::new();
    f.send("work");
    let delivery = f.claim(Resume::Rebuilt);
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    let mut publish = reply(&delivery);
    publish.outputs.push(Output {
        id: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        kind: OutputKind::Page,
        reference: serde_json::json!({"page":"fixture"}),
    });
    assert_eq!(
        f.db.publish_reply(&publish, 3).err(),
        Some(Error::InvalidRequest)
    );
    assert_eq!(f.count("messages"), 1);
    assert_eq!(f.db.status().unwrap().running_turns, 1);
    publish.outputs.clear();
    assert_eq!(f.db.publish_reply(&publish, 4).unwrap().answer.answered, 1);
}

#[test]
fn pause_accepts_work_and_authorization_precedes_receipt_replay() {
    let mut f = Fixture::new();
    let envelope = f.envelope("work");
    f.db.accept_message(&f.person, &envelope, 1).unwrap();
    let stranger = Person {
        sender: "stranger".into(),
        owner: "other".into(),
    };
    assert_eq!(
        f.db.accept_message(&stranger, &envelope, 1).err(),
        Some(Error::NotAllowed)
    );
    assert!(f.db.read_messages(&stranger, f.thread, 0).is_err());
    assert!(
        f.db.read_messages_for_bot(Uuid::new_v4(), f.thread, 0)
            .is_err()
    );
    f.db.connection
        .execute("UPDATE bots SET paused=1", [])
        .unwrap();
    assert!(
        f.db.claim_messages(
            f.bot,
            f.thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            64 * 1024,
            2
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(f.count("inbox"), 1);
}

#[test]
fn receipt_uses_existing_wire_codec_and_object_order_does_not_change_identity() {
    let mut f = Fixture::new();
    let mut envelope = f.envelope("work");
    envelope.references = vec![serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap()];
    let first = f.db.accept_message(&f.person, &envelope, 1).unwrap();
    envelope.references = vec![serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap()];
    let replay = f.db.accept_message(&f.person, &envelope, 2).unwrap();
    assert!(replay.replayed);
    let frame = koru_bot_client_wire::encode_body(&replay.wire(envelope.id).unwrap()).unwrap();
    let koru_bot_client_wire::WireMessage::Receipt(receipt) =
        koru_bot_client_wire::decode_body(&frame).unwrap()
    else {
        panic!("receipt")
    };
    assert!(receipt.replayed);
    assert_eq!(
        receipt.response.unwrap().parse::<Accepted>().unwrap(),
        first.answer
    );
}

#[test]
fn catchup_is_bounded_moves_only_on_acceptance_and_includes_each_speaker() {
    let mut f = Fixture::new();
    let content = Content {
        body: "context".into(),
        attachments: vec![],
        references: vec![],
        in_reply_to: None,
        deadline_ms: None,
        reply_to: ReplyTo::Thread,
    };
    for sequence in 1..=250 {
        f.db.connection
            .execute(
                "INSERT INTO messages VALUES (?1,?2,?3,?4,'words',?5,'published',0)",
                params![
                    Uuid::new_v4().to_string(),
                    f.thread.to_string(),
                    sequence,
                    if sequence % 2 == 0 {
                        "person"
                    } else {
                        "another participant"
                    },
                    encode(&content).unwrap()
                ],
            )
            .unwrap();
    }
    f.send("new work");
    let delivery = f.claim(Resume::Rebuilt);
    assert_eq!(delivery.context.len() + delivery.work.len(), HISTORY_LIMIT);
    assert!(delivery.older_history_available);
    assert!(delivery.context.iter().any(|m| m.sender == "person"));
    assert!(
        delivery
            .context
            .iter()
            .any(|m| m.sender == "another participant")
    );
    let point = |f: &Fixture| {
        f.db.connection
            .query_row(
                "SELECT read_sequence FROM participants WHERE kind='bot'",
                [],
                |r| r.get::<_, u64>(0),
            )
            .unwrap()
    };
    assert_eq!(point(&f), 0);
    f.db.delivery_accepted(delivery.turn, delivery.id).unwrap();
    assert_eq!(point(&f), 251);
    assert_eq!(
        f.db.read_messages(&f.person, f.thread, 0).unwrap().len(),
        PAGE_LIMIT
    );
}
