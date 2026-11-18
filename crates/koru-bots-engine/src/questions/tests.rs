// SPDX-License-Identifier: MPL-2.0
use super::*;
struct Fixture {
    db: Database,
    who: Person,
    thread: Uuid,
    turn: Turn,
    transient: Transient,
}
impl Fixture {
    fn new() -> Self {
        let mut db = Database::in_memory().unwrap();
        let bot = Uuid::new_v4();
        let thread = Uuid::new_v4();
        let worker = Uuid::new_v4();
        let session = Uuid::new_v4();
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection
            .execute(
                "INSERT INTO workers VALUES (?1,?2,1,1)",
                params![bot.to_string(), worker.to_string()],
            )
            .unwrap();
        db.connection.execute("INSERT INTO credentials(digest,scope,bot,thread,session,worker_incarnation,grant) VALUES (?1,'owner',?2,?3,?4,?5,x'7b7d')",params![vec![1u8;32],bot.to_string(),thread.to_string(),session.to_string(),worker.to_string()]).unwrap();
        let who = Person {
            owner: "owner".into(),
            sender: "person".into(),
        };
        db.accept_message(
            &who,
            &messages::Envelope {
                id: Uuid::new_v4(),
                to: bot,
                thread: Some(thread),
                in_reply_to: None,
                body: "work".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            },
            1,
        )
        .unwrap();
        let delivery = db
            .claim_messages(
                bot,
                thread,
                worker,
                session,
                messages::Resume::Rebuilt,
                64 * 1024,
                2,
            )
            .unwrap()
            .unwrap();
        db.delivery_accepted(delivery.turn, delivery.id).unwrap();
        Self {
            db,
            who,
            thread,
            turn: delivery.turn,
            transient: Transient::default(),
        }
    }
    fn ask(&mut self, secret: bool) -> Uuid {
        let key = protocol::QuestionRequestId::mint();
        self.db
            .question_event(
                self.turn,
                &protocol::AgentEvent::QuestionsRequired {
                    turn_id: self.turn.id.to_string().parse().unwrap(),
                    request_id: key,
                    questions: vec![protocol::AgentQuestion {
                        id: "question".into(),
                        header: String::new(),
                        question: "synthetic question".into(),
                        options: vec![],
                        allow_other: true,
                        secret,
                    }],
                    blocking: true,
                    delivery: protocol::QuestionsDelivery::InTurn,
                },
                3,
            )
            .unwrap();
        key.as_uuid()
    }
    fn state(&self) -> String {
        self.db
            .connection
            .query_row(
                "SELECT state FROM turns WHERE id=?1",
                [self.turn.id.to_string()],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn answer(&mut self, id: Uuid, command: Uuid, text: &str) -> Result<Receipt<Accepted>, Error> {
        self.db.answer_question(
            &self.who,
            command,
            id,
            &Answer::Questions {
                answers: Some(vec![protocol::QuestionAnswer {
                    question_id: "question".into(),
                    value: protocol::QuestionAnswerValue::Text { text: text.into() },
                }]),
            },
            &mut self.transient,
            4,
        )
    }
    fn count(&self, table: &str) -> u64 {
        self.db
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn question_and_answer_are_durable_idempotent_and_resume_the_exact_turn() {
    let mut f = Fixture::new();
    let q = f.ask(false);
    let command = Uuid::new_v4();
    assert_eq!(f.state(), "needs_you");
    assert_eq!(f.db.read_questions(&f.who, f.thread).unwrap().len(), 1);
    assert!(!f.answer(q, command, "answer").unwrap().answer.continued);
    assert_eq!(f.state(), "running");
    assert!(f.answer(q, command, "answer").unwrap().replayed);
    assert_eq!(f.answer(q, command, "changed").err(), Some(Error::Conflict));
    let pending = f.db.pending_question_answers(f.turn, &f.transient).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].command, command);
    f.db.question_answer_sent(f.turn, q, command).unwrap();
    assert!(
        f.db.pending_question_answers(f.turn, &f.transient)
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.count("questions"), 1);
    assert_eq!(f.count("messages"), 3); // request, question, person's answer
}
#[test]
fn secret_answer_exists_only_in_transient_delivery_and_never_in_database_blobs() {
    let mut f = Fixture::new();
    let q = f.ask(true);
    let command = Uuid::new_v4();
    f.answer(q, command, "private-fixture-secret").unwrap();
    let delivered = f.db.pending_question_answers(f.turn, &f.transient).unwrap();
    assert!(
        String::from_utf8(bytes(&delivered[0].answer).unwrap())
            .unwrap()
            .contains("private-fixture-secret")
    );
    for (table, column) in [
        ("questions", "answer"),
        ("questions", "prompt"),
        ("messages", "payload"),
        ("receipts", "answer"),
    ] {
        let values =
            f.db.connection
                .prepare(&format!(
                    "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
                ))
                .unwrap()
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        assert!(
            values
                .iter()
                .all(|v| !String::from_utf8_lossy(v).contains("private-fixture-secret"))
        );
    }
    f.db.question_answer_sent(f.turn, q, command).unwrap();
    f.transient.prune(&f.db);
    assert!(f.transient.answers.is_empty());
}
#[test]
fn restart_preserves_unanswered_question_and_plain_answer_continues_as_work() {
    let mut f = Fixture::new();
    let q = f.ask(false);
    f.db.recover_interrupted_turns().unwrap();
    assert_eq!(f.state(), "needs_you");
    assert_eq!(f.db.read_questions(&f.who, f.thread).unwrap().len(), 1);
    let command = Uuid::new_v4();
    assert!(
        f.answer(q, command, "after restart")
            .unwrap()
            .answer
            .continued
    );
    assert_eq!(f.state(), "interrupted");
    assert_eq!(f.count("inbox"), 2);
    f.answer(q, command, "after restart").unwrap();
    assert_eq!(f.count("inbox"), 2);
}
#[test]
fn accepted_but_unsent_answer_is_recovered_once_and_lost_secret_is_not_reconstructed() {
    for secret in [false, true] {
        let mut f = Fixture::new();
        let q = f.ask(secret);
        f.answer(q, Uuid::new_v4(), "synthetic answer").unwrap();
        f.db.recover_interrupted_turns().unwrap();
        f.transient = Transient::default();
        assert_eq!(f.count("inbox"), 2);
        assert_eq!(f.state(), "interrupted");
        f.db.recover_interrupted_turns().unwrap();
        assert_eq!(f.count("inbox"), 2);
        let count:u64=f.db.connection.query_row("SELECT count(*) FROM messages WHERE CAST(payload AS TEXT) LIKE '%secret_answer_not_delivered%'",[],|r|r.get(0)).unwrap();
        assert_eq!(count, u64::from(secret));
    }
}
#[test]
fn stop_wrong_owner_and_invalid_answer_never_queue_provider_work() {
    let mut f = Fixture::new();
    let q = f.ask(false);
    let wrong = Person {
        owner: "other".into(),
        sender: "person".into(),
    };
    assert_eq!(
        f.db.read_questions(&wrong, f.thread).err(),
        Some(Error::NotAllowed)
    );
    assert_eq!(
        f.db.answer_question(
            &f.who,
            Uuid::new_v4(),
            q,
            &Answer::Approval {
                decision: protocol::ApprovalDecision::Approve,
                choice: None
            },
            &mut f.transient,
            4
        )
        .err(),
        Some(Error::InvalidRequest)
    );
    f.db.stop_turn(&f.who, Uuid::new_v4(), f.turn, 4).unwrap();
    assert_eq!(
        f.answer(q, Uuid::new_v4(), "too late").err(),
        Some(Error::StaleTurn)
    );
    assert!(f.db.read_questions(&f.who, f.thread).unwrap().is_empty());
    assert_eq!(f.count("inbox"), 0);
}
#[test]
fn explicit_approval_choice_is_validated_and_provider_resolution_closes_it() {
    let mut f = Fixture::new();
    let key = protocol::ApprovalId::mint();
    let choices = protocol::one_time_approval_choices();
    let event = protocol::AgentEvent::ApprovalRequired {
        turn_id: f.turn.id.to_string().parse().unwrap(),
        approval_id: key,
        summary: "synthetic approval".into(),
        choices: choices.clone(),
    };
    f.db.question_event(f.turn, &event, 3).unwrap();
    f.db.question_event(f.turn, &event, 3).unwrap();
    assert_eq!(f.count("questions"), 1);
    assert_eq!(f.count("messages"), 2);
    let wrong = Answer::Approval {
        decision: protocol::ApprovalDecision::Approve,
        choice: Some(protocol::ApprovalChoiceId::mint()),
    };
    assert_eq!(
        f.db.answer_question(
            &f.who,
            Uuid::new_v4(),
            key.as_uuid(),
            &wrong,
            &mut f.transient,
            4
        )
        .err(),
        Some(Error::InvalidRequest)
    );
    let choice = choices.iter().find(|c| !c.intent.grants()).unwrap().id;
    let answer = Answer::Approval {
        decision: protocol::ApprovalDecision::Approve,
        choice: Some(choice),
    };
    f.db.answer_question(
        &f.who,
        Uuid::new_v4(),
        key.as_uuid(),
        &answer,
        &mut f.transient,
        4,
    )
    .unwrap();
    let pending = f.db.pending_question_answers(f.turn, &f.transient).unwrap();
    assert!(matches!(
        pending[0].answer,
        Answer::Approval {
            decision: protocol::ApprovalDecision::Reject,
            ..
        }
    ));
    f.db.question_event(
        f.turn,
        &protocol::AgentEvent::ApprovalResolved {
            turn_id: f.turn.id.to_string().parse().unwrap(),
            approval_id: key,
            decision: protocol::ApprovalDecision::Reject,
            choice: Some(choice),
        },
        5,
    )
    .unwrap();
    assert!(f.db.read_questions(&f.who, f.thread).unwrap().is_empty());
    assert_eq!(f.state(), "running");
}

#[test]
fn offline_secret_form_can_restart_its_request_without_storing_the_secret() {
    let mut f = Fixture::new();
    let q = f.ask(true);
    f.db.recover_interrupted_turns().unwrap();
    assert!(!f.db.read_questions(&f.who, f.thread).unwrap()[0].provider_available);
    assert_eq!(
        f.answer(q, Uuid::new_v4(), "never stored").err(),
        Some(Error::EngineUnavailable)
    );
    reask(&f.db.connection, &f.who, f.thread, q, 5).unwrap();
    assert_eq!(f.state(), "interrupted");
    assert!(f.db.read_questions(&f.who, f.thread).unwrap().is_empty());
    assert_eq!(f.count("inbox"), 1);
}

#[test]
fn several_blocking_questions_stay_blocked_until_all_are_answered_after_restart() {
    let mut f = Fixture::new();
    let first = f.ask(false);
    let second = f.ask(false);
    f.db.recover_interrupted_turns().unwrap();
    f.answer(first, Uuid::new_v4(), "one").unwrap();
    assert_eq!(f.state(), "needs_you");
    f.answer(second, Uuid::new_v4(), "two").unwrap();
    assert_eq!(f.state(), "interrupted");
}

#[test]
fn failed_history_insert_rolls_back_answer_receipt_and_transient_secret() {
    let mut f = Fixture::new();
    let q = f.ask(true);
    f.db.connection.execute_batch("CREATE TEMP TRIGGER fail_answer BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.answer(q, Uuid::new_v4(), "fixture").is_err());
    assert_eq!(f.state(), "needs_you");
    assert!(f.transient.answers.is_empty());
    assert_eq!(f.count("receipts"), 1);
}
