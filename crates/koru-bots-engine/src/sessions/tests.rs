// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    Database,
    messages::{Envelope, Person, ReplyTo},
};
use koru_session_protocol::{
    AgentTurnId, ApprovalChoiceId, ApprovalDecision, ApprovalId, ProviderResumeToken,
};
use koru_session_runtime::providers::*;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Steering {
    Accepted,
    Rejected,
    Lost,
}

#[derive(Clone, Default)]
struct Plan {
    fail_resume: bool,
    lost: bool,
    fail_retirement: bool,
    no_resume: bool,
    hold: bool,
    cancelled: bool,
    steering: Option<Steering>,
    question: Option<bool>,
}
#[derive(Default)]
struct Records {
    plans: VecDeque<Plan>,
    inputs: Vec<String>,
    histories: std::collections::BTreeMap<String, Vec<[u8; 32]>>,
    credentials: Vec<String>,
    starts: Vec<bool>,
    stopped: usize,
    steers: Vec<String>,
    answers: Vec<Vec<koru_session_protocol::QuestionAnswer>>,
}
struct Factory(Arc<Mutex<Records>>);
struct Wake;
impl ProviderWake for Wake {
    fn wake(&self) {}
}
impl ManagedProviderFactory for Factory {
    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        let mut records = self.0.lock().unwrap();
        records.credentials.push(
            request
                .delegation
                .unwrap()
                .credential
                .unwrap()
                .as_str()
                .into(),
        );
        let plan = records.plans.pop_front().unwrap_or_default();
        Ok(ManagedProvider {
            control: Box::new(Agent {
                records: self.0.clone(),
                plan,
                hashes: vec![],
                history: Uuid::new_v4().to_string(),
                events: VecDeque::new(),
                caps: None,
                question: None,
            }),
            wake: Arc::new(Wake),
        })
    }
}
struct Agent {
    records: Arc<Mutex<Records>>,
    plan: Plan,
    hashes: Vec<[u8; 32]>,
    history: String,
    events: VecDeque<AgentEvent>,
    caps: Option<AgentCapabilities>,
    question: Option<(
        koru_session_protocol::QuestionRequestId,
        AgentTurnId,
        Vec<koru_session_protocol::AgentQuestion>,
    )>,
}
fn failure() -> ProviderFailure {
    ProviderFailure::new(
        koru_session_protocol::ErrorCode::ProviderFailed,
        ProviderStage::Read,
        ProviderDiagnostic::UnexpectedExit,
    )
}
impl AgentControl for Agent {
    fn start(&mut self, start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        self.records
            .lock()
            .unwrap()
            .starts
            .push(start.resume.is_some());
        if start.resume.is_some() && self.plan.fail_resume {
            return Err(failure());
        }
        if let Some(token) = &start.resume {
            self.history = token.as_str().into();
            self.hashes = self
                .records
                .lock()
                .unwrap()
                .histories
                .get(&self.history)
                .cloned()
                .unwrap_or_default();
        }
        let caps = AgentCapabilities {
            provider: if self.plan.steering.is_some() {
                "claude"
            } else {
                "scripted"
            }
            .parse()
            .unwrap(),
            executable_version: if self.plan.steering.is_some() {
                "0.76.0"
            } else {
                "1"
            }
            .parse()
            .unwrap(),
            protocol_version: "1".parse().unwrap(),
            capabilities: Default::default(),
            delegation: DelegationFacts::undeclared(),
        };
        self.caps = Some(caps.clone());
        Ok(caps)
    }
    fn capabilities(&self) -> Option<&AgentCapabilities> {
        self.caps.as_ref()
    }
    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        {
            let mut records = self.records.lock().unwrap();
            records
                .histories
                .entry(self.history.clone())
                .or_default()
                .push(*blake3::hash(input.text.as_bytes()).as_bytes());
            records.inputs.push(input.text);
        }
        self.events.push_back(AgentEvent::TurnStarted {
            turn_id: input.turn_id,
        });
        if self.plan.cancelled {
            self.events.push_back(AgentEvent::TurnCancelled {
                turn_id: input.turn_id,
                reason: koru_session_protocol::CancelReason::Run,
            });
        } else if let Some(secret) = self.plan.question {
            let request_id = koru_session_protocol::QuestionRequestId::mint();
            let questions = vec![koru_session_protocol::AgentQuestion {
                id: "q".into(),
                header: String::new(),
                question: "Synthetic question".into(),
                options: vec![],
                allow_other: true,
                secret,
            }];
            self.question = Some((request_id, input.turn_id, questions.clone()));
            self.events.push_back(AgentEvent::QuestionsRequired {
                turn_id: input.turn_id,
                request_id,
                questions,
                blocking: true,
                delivery: koru_session_protocol::QuestionsDelivery::InTurn,
            });
        } else if !self.plan.lost && !self.plan.hold {
            self.events.push_back(AgentEvent::MessageDelta {
                turn_id: input.turn_id,
                text: "synthetic reply".into(),
            });
            self.events.push_back(AgentEvent::TurnCompleted {
                turn_id: input.turn_id,
            });
        }
        Ok(())
    }
    fn steer(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        let mut records = self.records.lock().unwrap();
        records.steers.push(input.text.clone());
        match self.plan.steering {
            Some(Steering::Accepted | Steering::Lost) => {
                records
                    .histories
                    .entry(self.history.clone())
                    .or_default()
                    .push(*blake3::hash(input.text.as_bytes()).as_bytes());
                if matches!(self.plan.steering, Some(Steering::Lost)) {
                    Err(failure())
                } else {
                    Ok(())
                }
            }
            _ => Err(ProviderFailure::new(
                koru_session_protocol::ErrorCode::InvalidTransition,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidState,
            )),
        }
    }
    fn next_event(&mut self, _: Duration) -> Result<Option<AgentEvent>, ProviderFailure> {
        if self.plan.lost && self.events.is_empty() {
            return Err(failure());
        }
        if self.plan.hold && self.events.is_empty() {
            std::thread::yield_now();
        }
        Ok(self.events.pop_front())
    }
    fn cancel(&mut self, _: AgentTurnId) -> Result<(), ProviderFailure> {
        Ok(())
    }
    fn answer_approval(
        &mut self,
        _: ApprovalId,
        _: ApprovalDecision,
        _: Option<ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure> {
        Ok(())
    }
    fn answer_questions(
        &mut self,
        request: koru_session_protocol::QuestionRequestId,
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        let (expected, turn_id, questions) = self.question.take().ok_or_else(failure)?;
        if request != expected {
            return Err(failure());
        }
        let answers = answers.unwrap_or_default();
        self.records.lock().unwrap().answers.push(answers.clone());
        self.events.push_back(AgentEvent::QuestionsResolved {
            turn_id,
            request_id: request,
            outcome: koru_session_protocol::QuestionsOutcome::Answered {
                answers: koru_session_protocol::withhold_secrets(&questions, &answers),
            },
        });
        self.events.push_back(AgentEvent::MessageDelta {
            turn_id,
            text: "finished".into(),
        });
        self.events.push_back(AgentEvent::TurnCompleted { turn_id });
        Ok(())
    }
    fn diagnostics(&self) -> ProviderDiagnostics {
        Default::default()
    }
    fn resume_token(&self) -> Option<ProviderResumeToken> {
        (!self.plan.no_resume).then(|| self.history.parse().unwrap())
    }
    fn replayed_input_hashes(&self) -> &[[u8; 32]] {
        &self.hashes
    }
    fn shutdown(&mut self) -> Result<(), ProviderFailure> {
        self.records.lock().unwrap().stopped += 1;
        if self.plan.fail_retirement {
            Err(failure())
        } else {
            Ok(())
        }
    }
}

struct TestPort {
    db: Mutex<Database>,
    bot: Uuid,
    worker: Uuid,
    context: Mutex<Option<String>>,
    offers: Mutex<Vec<Delivery>>,
    events: Mutex<Vec<AgentEvent>>,
    issued: Mutex<std::collections::BTreeMap<Uuid, Uuid>>,
    fail_publish: AtomicBool,
    steering_message: Mutex<Option<Envelope>>,
    question_answers: Mutex<crate::questions::Transient>,
}
impl Port for TestPort {
    fn context(&self, _: Uuid) -> Result<Context, Error> {
        let text = self
            .context
            .lock()
            .unwrap()
            .clone()
            .ok_or(Error::NotAllowed)?;
        Ok(Context {
            role: Some(Page { id: self.bot, text }),
            knowledge: vec![],
            memory: None,
        })
    }
    fn credential(&self, thread: Uuid, _: Uuid) -> Result<SessionGrant, Error> {
        let session = SessionId::mint();
        let db = self.db.lock().unwrap();
        db.connection.execute("INSERT OR IGNORE INTO workers(bot,incarnation,started_at_ms,last_heard_at_ms) VALUES (?1,?2,1,1)",rusqlite::params![self.bot.to_string(),self.worker.to_string()])?;
        db.connection.execute("INSERT INTO credentials(digest,scope,bot,thread,session,worker_incarnation,grant) VALUES (?1,'owner',?2,?3,?4,?5,x'7b7d')",rusqlite::params![blake3::hash(session.to_string().as_bytes()).as_bytes().as_slice(),self.bot.to_string(),thread.to_string(),session.to_string(),self.worker.to_string()])?;
        drop(db);
        self.issued
            .lock()
            .unwrap()
            .insert(thread, session.as_uuid());
        Ok(SessionGrant {
            session,
            launch: DelegationLaunch {
                model: None,
                mcp_endpoint: "http://127.0.0.1:1/mcp".parse().unwrap(),
                credential: Some(session.to_string().parse().unwrap()),
                tools: Default::default(),
                resume: None,
            },
        })
    }
    fn claim(
        &self,
        thread: Uuid,
        session: Uuid,
        resume: Resume,
        budget: usize,
    ) -> Result<Option<Delivery>, Error> {
        assert_eq!(
            self.issued.lock().unwrap().get(&thread),
            Some(&session),
            "claim must use the engine-issued incarnation"
        );
        let delivery = self.db.lock().unwrap().claim_messages(
            self.bot,
            thread,
            self.worker,
            session,
            resume,
            budget,
            10,
        )?;
        if let Some(delivery) = &delivery {
            self.offers.lock().unwrap().push(delivery.clone());
        }
        Ok(delivery)
    }
    fn accepted(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.db.lock().unwrap().delivery_accepted(turn, delivery)
    }
    fn unknown(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.db
            .lock()
            .unwrap()
            .mark_delivery_unknown(turn, delivery)
    }
    fn event(&self, turn: Turn, event: &AgentEvent) -> Result<(), Error> {
        if matches!(
            event,
            AgentEvent::QuestionsRequired { .. } | AgentEvent::QuestionsResolved { .. }
        ) {
            let mut db = self.db.lock().unwrap();
            db.question_event(turn, event, 10)?;
            if let AgentEvent::QuestionsRequired { request_id, .. } = event {
                db.answer_question(
                    &Person {
                        owner: "owner".into(),
                        sender: "person".into(),
                    },
                    Uuid::new_v4(),
                    request_id.as_uuid(),
                    &crate::questions::Answer::Questions {
                        answers: Some(vec![koru_session_protocol::QuestionAnswer {
                            question_id: "q".into(),
                            value: koru_session_protocol::QuestionAnswerValue::Text {
                                text: "synthetic answer".into(),
                            },
                        }]),
                    },
                    &mut self.question_answers.lock().unwrap(),
                    10,
                )?;
            }
        }
        if matches!(event, AgentEvent::MessageDelta { .. })
            && let Some(message) = self.steering_message.lock().unwrap().take()
        {
            self.db.lock().unwrap().accept_message(
                &Person {
                    owner: "owner".into(),
                    sender: "person".into(),
                },
                &message,
                10,
            )?;
        }
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    fn answers(&self, turn: Turn) -> Result<Vec<crate::questions::Delivery>, Error> {
        self.db
            .lock()
            .unwrap()
            .pending_question_answers(turn, &self.question_answers.lock().unwrap())
    }
    fn answer_sent(&self, turn: Turn, question: Uuid, command: Uuid) -> Result<(), Error> {
        self.db
            .lock()
            .unwrap()
            .question_answer_sent(turn, question, command)
    }
    fn offer_steer(&self, turn: Turn, budget: usize) -> Result<Option<Delivery>, Error> {
        self.db.lock().unwrap().offer_steer(turn, budget, 10)
    }
    fn reject_steer(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.db.lock().unwrap().reject_steer(turn, delivery)
    }
    fn publish(&self, turn: Turn, command: Uuid, body: String) -> Result<(), Error> {
        if self.fail_publish.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::StorageUnavailable);
        }
        self.db.lock().unwrap().publish_reply(
            &crate::messages::Publish {
                command,
                turn,
                body,
                outputs: vec![],
            },
            11,
        )?;
        Ok(())
    }
    fn retired(&self, turn: Turn) -> Result<(), Error> {
        self.db.lock().unwrap().interrupt_turn(turn)
    }
}
struct Fixture {
    port: Arc<TestPort>,
    records: Arc<Mutex<Records>>,
    database: Arc<Mutex<BotDatabase>>,
    thread: Uuid,
}
impl Fixture {
    fn new() -> Self {
        let db = Database::in_memory().unwrap();
        let bot = Uuid::new_v4();
        let thread = Uuid::new_v4();
        db.connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",rusqlite::params![thread.to_string(),bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",rusqlite::params![thread.to_string(),bot.to_string()]).unwrap();
        let f = Self {
            port: Arc::new(TestPort {
                db: Mutex::new(db),
                bot,
                worker: Uuid::new_v4(),
                context: Mutex::new(Some("current role".into())),
                offers: Mutex::new(vec![]),
                events: Mutex::new(vec![]),
                issued: Mutex::new(Default::default()),
                fail_publish: AtomicBool::new(false),
                steering_message: Mutex::new(None),
                question_answers: Mutex::new(Default::default()),
            }),
            records: Arc::new(Mutex::new(Records::default())),
            database: Arc::new(Mutex::new(BotDatabase::in_memory().unwrap())),
            thread,
        };
        f.port
            .db
            .lock()
            .unwrap()
            .accept_message(
                &Person {
                    owner: "owner".into(),
                    sender: "person".into(),
                },
                &Envelope {
                    id: Uuid::new_v4(),
                    to: bot,
                    thread: Some(thread),
                    in_reply_to: None,
                    body: "owed synthetic work".into(),
                    attachments: vec![],
                    references: vec![],
                    deadline_ms: None,
                    reply_to: ReplyTo::Thread,
                },
                1,
            )
            .unwrap();
        f
    }
    fn start(&self) -> Start {
        Start {
            thread: self.thread,
            worker: self.port.worker,
            request: ManagedProviderRequest {
                provider: "scripted".parse().unwrap(),
                executable: "never-launched".into(),
                working_directory: std::env::current_dir().unwrap(),
                delegation: None,
                selection: None,
                mcp_servers: vec![],
                questions: false,
            },
            factory: Arc::new(Factory(self.records.clone())),
            port: self.port.clone(),
        }
    }
    fn run(&self) -> Result<(), Failure> {
        let (_, commands) = mpsc::sync_channel(1);
        run::run(
            self.start(),
            self.database.clone(),
            Arc::new(AtomicBool::new(false)),
            commands,
        )
    }
    fn plan(&self, plan: Plan) {
        self.records.lock().unwrap().plans.push_back(plan);
    }
    fn input(&self, index: usize) -> serde_json::Value {
        serde_json::from_str(&self.records.lock().unwrap().inputs[index]).unwrap()
    }
}

#[test]
fn provider_questions_deliver_once_and_finish_without_journaling_secret_text() {
    for secret in [false, true] {
        let f = Fixture::new();
        f.plan(Plan {
            question: Some(secret),
            ..Default::default()
        });
        let mut pool = Pool::new(f.port.worker, BotDatabase::in_memory().unwrap()).unwrap();
        pool.admit(f.start()).unwrap();
        let done = finish(&mut pool);
        assert!(done.result.is_ok(), "{:?}", done.result);
        assert_eq!(f.records.lock().unwrap().answers.len(), 1);
        assert!(
            f.port
                .db
                .lock()
                .unwrap()
                .read_questions(
                    &Person {
                        owner: "owner".into(),
                        sender: "person".into()
                    },
                    f.thread
                )
                .unwrap()
                .is_empty()
        );
        if secret {
            assert!(
                !serde_json::to_string(&*f.port.events.lock().unwrap())
                    .unwrap()
                    .contains("synthetic answer")
            );
            let payloads = f
                .port
                .db
                .lock()
                .unwrap()
                .connection
                .prepare("SELECT payload FROM messages")
                .unwrap()
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                payloads
                    .iter()
                    .all(|v| !String::from_utf8_lossy(v).contains("synthetic answer"))
            );
        }
    }
}

#[test]
fn an_unqualified_route_leaves_input_for_the_next_turn() {
    let f = Fixture::new();
    *f.port.steering_message.lock().unwrap() = Some(Envelope {
        id: Uuid::new_v4(),
        to: f.port.bot,
        thread: Some(f.thread),
        in_reply_to: None,
        body: "wait for next turn".into(),
        attachments: vec![],
        references: vec![],
        deadline_ms: None,
        reply_to: ReplyTo::Thread,
    });
    f.run().unwrap();
    assert!(f.records.lock().unwrap().steers.is_empty());
    f.run().unwrap();
    assert_eq!(
        f.input(1)["delivery"]["work"][0]["content"]["body"],
        "wait for next turn"
    );
}

#[test]
fn native_steer_confirmation_and_lost_confirmation_preserve_delivery_evidence() {
    for outcome in [Steering::Accepted, Steering::Rejected, Steering::Lost] {
        let f = Fixture::new();
        f.plan(Plan {
            steering: Some(outcome),
            ..Default::default()
        });
        *f.port.steering_message.lock().unwrap() = Some(Envelope {
            id: Uuid::new_v4(),
            to: f.port.bot,
            thread: Some(f.thread),
            in_reply_to: None,
            body: "new steering request".into(),
            attachments: vec![],
            references: vec![],
            deadline_ms: None,
            reply_to: ReplyTo::Thread,
        });
        let result = f.run();
        assert_eq!(result.is_err(), matches!(outcome, Steering::Lost));
        assert_eq!(f.records.lock().unwrap().steers.len(), 1);
        let owed: u64 = f
            .port
            .db
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT count(*) FROM obligations WHERE state IN ('owed','delivered')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        match outcome {
            Steering::Accepted => assert_eq!(owed, 0),
            Steering::Rejected => {
                assert_eq!(owed, 1);
                f.run().unwrap();
                assert_eq!(
                    f.input(1)["delivery"]["work"][0]["content"]["body"],
                    "new steering request"
                );
            }
            Steering::Lost => {
                assert_eq!(owed, 2);
                f.run().unwrap();
                let input = f.input(1);
                assert!(input["delivery"]["work"].as_array().unwrap().is_empty());
                assert_eq!(
                    input["delivery"]["resume_accepted"]
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );
            }
        }
    }
}

#[test]
fn unexpected_provider_cancellation_preserves_owed_work_and_confirms_shutdown() {
    let f = Fixture::new();
    f.plan(Plan {
        cancelled: true,
        ..Default::default()
    });
    assert!(matches!(f.run(), Err(Failure::ProviderEnded)));
    assert_eq!(f.records.lock().unwrap().stopped, 1);
    assert_eq!(
        f.port
            .db
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT count(*) FROM obligations WHERE state IN ('owed','delivered')",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        f.port
            .db
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT count(*) FROM messages WHERE sender=?1",
                [f.port.bot.to_string()],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn lost_acceptance_is_reconciled_from_exact_replay_without_resending_work() {
    let f = Fixture::new();
    f.plan(Plan {
        lost: true,
        ..Default::default()
    });
    assert!(f.run().is_err());
    let first = f.port.offers.lock().unwrap()[0].id;
    f.run().unwrap();
    let input = f.input(1);
    assert!(input["delivery"]["work"].as_array().unwrap().is_empty());
    assert_eq!(input["delivery"]["resume_accepted"][0], first.to_string());
    assert_eq!(input["context"], serde_json::Value::Null);
    assert_eq!(f.records.lock().unwrap().starts, vec![false, true]);
    // Completion publishes before retirement; the obligation is answered.
    assert_eq!(
        f.port
            .db
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT count(*) FROM obligations WHERE state='answered'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        f.port
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageDelta { .. }))
            .count(),
        1
    );
    f.run().unwrap();
    assert_eq!(
        f.records.lock().unwrap().inputs.len(),
        2,
        "answered work must not be claimed again"
    );
}

#[test]
fn missing_input_replay_resends_current_context_as_well_as_owed_work() {
    let f = Fixture::new();
    f.plan(Plan {
        lost: true,
        ..Default::default()
    });
    assert!(f.run().is_err());
    // The journal was saved, but the provider never retained this input.
    f.records.lock().unwrap().inputs.clear();
    f.records.lock().unwrap().histories.clear();
    f.run().unwrap();
    assert_eq!(f.input(0)["context"]["role"]["text"], "current role");
    assert_eq!(f.input(0)["delivery"]["work"].as_array().unwrap().len(), 1);
    assert!(f.input(0)["delivery"]["uncertain"].as_bool().unwrap());
}

#[test]
fn two_conversations_have_independent_sessions_and_retirement() {
    let f = Fixture::new();
    let thread = Uuid::new_v4();
    {
        let mut db = f.port.db.lock().unwrap();
        db.connection.execute("INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'conversation','','active')",rusqlite::params![thread.to_string(),f.port.bot.to_string()]).unwrap();
        db.connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",rusqlite::params![thread.to_string(),f.port.bot.to_string()]).unwrap();
        db.accept_message(
            &Person {
                owner: "owner".into(),
                sender: "person".into(),
            },
            &Envelope {
                id: Uuid::new_v4(),
                to: f.port.bot,
                thread: Some(thread),
                in_reply_to: None,
                body: "second thread".into(),
                attachments: vec![],
                references: vec![],
                deadline_ms: None,
                reply_to: ReplyTo::Thread,
            },
            1,
        )
        .unwrap();
    }
    f.plan(Plan {
        hold: true,
        ..Default::default()
    });
    let mut pool = Pool::new(f.port.worker, BotDatabase::in_memory().unwrap()).unwrap();
    pool.admit(f.start()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.records.lock().unwrap().inputs.is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let mut second = f.start();
    second.thread = thread;
    pool.admit(second).unwrap();
    assert_eq!(pool.occupied(), 2);
    let done = finish(&mut pool);
    assert_eq!(done.thread, thread);
    assert!(done.retired);
    assert_eq!(pool.occupied(), 1);
    let db = f.port.db.lock().unwrap();
    let running: u64 = db
        .connection
        .query_row(
            "SELECT count(*) FROM turns WHERE thread=?1 AND state='running'",
            [f.thread.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(running, 1);
    drop(db);
    pool.stop(f.thread);
    assert!(finish(&mut pool).retired);
    assert_eq!(pool.occupied(), 0);
}

#[test]
fn failed_resume_retires_first_and_rebuilds_with_new_credentials_and_current_pages() {
    let f = Fixture::new();
    f.plan(Plan {
        lost: true,
        ..Default::default()
    });
    assert!(f.run().is_err());
    *f.port.context.lock().unwrap() = Some("changed role".into());
    f.plan(Plan {
        fail_resume: true,
        ..Default::default()
    });
    f.plan(Plan::default());
    f.run().unwrap();
    assert_eq!(f.records.lock().unwrap().starts, vec![false, true, false]);
    assert_eq!(f.records.lock().unwrap().stopped, 3);
    let records = f.records.lock().unwrap();
    assert_eq!(
        records
            .credentials
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    drop(records);
    assert_eq!(f.input(1)["context"]["role"]["text"], "changed role");
    assert_eq!(f.input(1)["delivery"]["work"].as_array().unwrap().len(), 1);
    assert!(f.input(1)["delivery"]["continuation"].as_bool().unwrap());
}

#[test]
fn unqualified_provider_rebuilds_and_unreadable_context_never_launches() {
    let f = Fixture::new();
    f.plan(Plan {
        no_resume: true,
        lost: true,
        ..Default::default()
    });
    assert!(f.run().is_err());
    f.run().unwrap();
    assert_eq!(f.records.lock().unwrap().starts, vec![false, false]);
    assert_eq!(f.input(1)["delivery"]["work"].as_array().unwrap().len(), 1);
    *f.port.context.lock().unwrap() = None;
    assert!(matches!(f.run(), Err(Failure::Engine(Error::NotAllowed))));
    assert_eq!(f.records.lock().unwrap().starts.len(), 2);
}

#[test]
fn current_context_and_delivery_share_one_input_budget() {
    let f = Fixture::new();
    *f.port.context.lock().unwrap() = Some("r".repeat(MAX_INPUT_BYTES - 4096));
    f.run().unwrap();
    assert!(f.records.lock().unwrap().inputs[0].len() <= MAX_INPUT_BYTES);
    let f = Fixture::new();
    *f.port.context.lock().unwrap() = Some("r".repeat(MAX_INPUT_BYTES));
    assert!(matches!(
        f.run(),
        Err(Failure::Engine(Error::InputTooSmall))
    ));
    assert!(f.port.offers.lock().unwrap().is_empty());
}

fn finish(pool: &mut Pool) -> Finished {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(done) = pool.finished().pop() {
            return done;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn pending_and_running_sessions_keep_worker_awake_and_duplicate_pair_is_refused() {
    let f = Fixture::new();
    f.plan(Plan {
        hold: true,
        ..Default::default()
    });
    let mut pool = Pool::new(f.port.worker, BotDatabase::in_memory().unwrap()).unwrap();
    pool.admit(f.start()).unwrap();
    assert_eq!(pool.occupied(), 1);
    assert_eq!(pool.admit(f.start()), Err(Error::Conflict));
    pool.stop(f.thread);
    assert!(finish(&mut pool).retired);
    assert_eq!(pool.occupied(), 0);
}

#[test]
fn unconfirmed_shutdown_keeps_slot_fenced_and_never_launches_rebuild() {
    let f = Fixture::new();
    f.run().unwrap();
    f.plan(Plan {
        fail_resume: true,
        fail_retirement: true,
        ..Default::default()
    });
    assert!(matches!(f.run(), Err(Failure::RetirementUnconfirmed)));
    assert_eq!(f.records.lock().unwrap().starts, vec![false, true]);
    let f = Fixture::new();
    f.plan(Plan {
        fail_retirement: true,
        ..Default::default()
    });
    let mut pool = Pool::new(f.port.worker, BotDatabase::in_memory().unwrap()).unwrap();
    pool.admit(f.start()).unwrap();
    assert!(!finish(&mut pool).retired);
    assert_eq!(pool.occupied(), 1);
    assert!(pool.finished().is_empty());
    assert_eq!(pool.admit(f.start()), Err(Error::Conflict));
}

#[test]
fn failed_publish_retires_without_answering_then_two_resumes_do_not_resend_work() {
    let f = Fixture::new();
    f.port
        .fail_publish
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        f.run(),
        Err(Failure::Engine(Error::StorageUnavailable))
    ));
    let original = f.port.offers.lock().unwrap()[0].id;
    assert!(f.run().is_err());
    f.port
        .fail_publish
        .store(false, std::sync::atomic::Ordering::Release);
    f.run().unwrap();
    for i in [1, 2] {
        let input = f.input(i);
        assert!(input["delivery"]["work"].as_array().unwrap().is_empty());
        assert_eq!(
            input["delivery"]["resume_accepted"][0],
            original.to_string()
        );
    }
    let mut db = f.port.db.lock().unwrap();
    let replies = db.read_messages_for_bot(f.port.bot, f.thread, 0).unwrap();
    assert_eq!(
        replies
            .iter()
            .filter(|message| message.content.body == "synthetic reply")
            .count(),
        1
    );
    assert!(
        db.claim_messages(
            f.port.bot,
            f.thread,
            f.port.worker,
            Uuid::new_v4(),
            Resume::Rebuilt,
            MAX_INPUT_BYTES,
            12
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn unreadable_or_obsolete_saved_state_rebuilds_from_owed_work() {
    for state in [
        b"invalid".to_vec(),
        encode(&Saved {
            version: 2,
            token: Some("obsolete".parse().unwrap()),
            context_hash: [0; 32],
            context_delivery: None,
            inputs: vec![],
        })
        .unwrap(),
    ] {
        let f = Fixture::new();
        f.database
            .lock()
            .unwrap()
            .save(&AgentState {
                thread: f.thread,
                session: Uuid::new_v4(),
                provider: "scripted".into(),
                resume_state: state,
                last_delivery: None,
                saved_at_ms: 1,
            })
            .unwrap();
        f.run().unwrap();
        assert_eq!(f.records.lock().unwrap().starts, vec![false]);
        assert_eq!(f.input(0)["delivery"]["work"].as_array().unwrap().len(), 1);
    }
}
