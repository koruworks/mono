// SPDX-License-Identifier: MPL-2.0

//! Explicit P1.4 provider proof. Normal cargo test skips without touching a
//! profile, disk, network or provider. Invoke with --run --root <new absolute
//! directory> --program <wrapper> --endpoint <empty synthetic loopback MCP>.

use koru_bots_engine::{
    Database, Error,
    bot_database::BotDatabase,
    messages::{self, Delivery, Resume, Turn},
    sessions::{self, Port},
};
use koru_session_protocol::{AgentEvent, DelegationLaunch};
use koru_session_runtime::{
    NoopAttachmentPathResolver,
    providers::{AcpFactory, AcpRoute, ManagedProviderRequest},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Boundary {
    engine: Mutex<Database>,
    bot: Uuid,
    worker: Uuid,
    endpoint: String,
    issued: AtomicUsize,
    offers: Mutex<Vec<Delivery>>,
    replies: AtomicUsize,
}
impl Port for Boundary {
    fn context(&self, _: Uuid) -> Result<sessions::Context, Error> {
        Ok(sessions::Context {
            role: Some(sessions::Page {
                id: self.bot,
                text: "This is a synthetic protocol test. Reply with only OK. Do not use tools."
                    .into(),
            }),
            knowledge: vec![],
            memory: None,
        })
    }
    fn credential(&self, _: Uuid, _: Uuid) -> Result<DelegationLaunch, Error> {
        let stage = if self.issued.fetch_add(1, Ordering::SeqCst) == 0 {
            "initial"
        } else {
            "replacement"
        };
        Ok(DelegationLaunch {
            model: None,
            mcp_endpoint: self.endpoint.parse().map_err(|_| Error::InvalidRequest)?,
            credential: Some(format!("synthetic-probe-{stage}").parse().unwrap()),
            tools: Default::default(),
            resume: None,
        })
    }
    fn claim(
        &self,
        thread: Uuid,
        session: Uuid,
        resume: Resume,
        budget: usize,
    ) -> Result<Option<Delivery>, Error> {
        let offer = self.engine.lock().unwrap().claim_messages(
            self.bot,
            thread,
            self.worker,
            session,
            resume,
            budget,
            2,
        )?;
        if let Some(offer) = &offer {
            self.offers.lock().unwrap().push(offer.clone());
        }
        Ok(offer)
    }
    fn accepted(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.engine
            .lock()
            .unwrap()
            .delivery_accepted(turn, delivery)
    }
    fn unknown(&self, turn: Turn, delivery: Uuid) -> Result<(), Error> {
        self.engine
            .lock()
            .unwrap()
            .mark_delivery_unknown(turn, delivery)
    }
    fn event(&self, _: Turn, event: &AgentEvent) -> Result<(), Error> {
        if matches!(event, AgentEvent::MessageDelta { .. }) {
            self.replies.fetch_add(1, Ordering::SeqCst);
        }
        if matches!(
            event,
            AgentEvent::ToolStarted { .. } | AgentEvent::ApprovalRequired { .. }
        ) {
            return Err(Error::NotAllowed);
        }
        Ok(())
    }
    fn retired(&self, turn: Turn) -> Result<(), Error> {
        self.engine.lock().unwrap().interrupt_turn(turn)
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_none_or(|arg| arg != "--run") {
        println!("SKIP real session proof: requires --run, a new root and synthetic loopback MCP");
        return;
    }
    assert_eq!(args.len(), 7);
    assert!(args[1] == "--root" && args[3] == "--program" && args[5] == "--endpoint");
    let root = PathBuf::from(&args[2]);
    assert!(root.is_absolute());
    assert!(args[6].starts_with("http://127.0.0.1:"));
    std::fs::create_dir(&root).expect("new disposable root");
    let mut engine = Database::open(&root).unwrap();
    let bot = Uuid::new_v4();
    let thread = Uuid::new_v4();
    let worker = Uuid::new_v4();
    let connection = rusqlite::Connection::open(root.join("bots/engine.db")).unwrap();
    connection.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'owner','synthetic',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
    connection
        .execute(
            "INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",
            rusqlite::params![thread.to_string(), bot.to_string()],
        )
        .unwrap();
    connection.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'person','person'),(?1,?2,'bot')",rusqlite::params![thread.to_string(),bot.to_string()]).unwrap();
    drop(connection);
    engine
        .accept_message(
            &messages::Person {
                sender: "person".into(),
                owner: "owner".into(),
            },
            &messages::Envelope {
                id: Uuid::new_v4(),
                to: bot,
                thread: Some(thread),
                body: "Reply with only OK. Do not call tools.".into(),
                attachments: vec![],
                references: vec![],
                in_reply_to: None,
                deadline_ms: None,
                reply_to: messages::ReplyTo::Thread,
            },
            1,
        )
        .unwrap();
    let boundary = Arc::new(Boundary {
        engine: Mutex::new(engine),
        bot,
        worker,
        endpoint: args[6].clone(),
        issued: AtomicUsize::new(0),
        offers: Mutex::new(vec![]),
        replies: AtomicUsize::new(0),
    });
    let mut route = AcpRoute::claude_code();
    route.session_new_meta = Some(serde_json::json!({"claudeCode":{"options":{
        "tools":[],"settingSources":[],"strictMcpConfig":true
    }}}));
    let factory = Arc::new(AcpFactory::new(route, Arc::new(NoopAttachmentPathResolver)));
    let mut pool = sessions::Pool::new(worker, BotDatabase::open(&root, bot).unwrap()).unwrap();
    for pass in 0..2 {
        let before = boundary.replies.load(Ordering::SeqCst);
        pool.admit(sessions::Start {
            thread,
            worker,
            factory: factory.clone(),
            port: boundary.clone(),
            request: ManagedProviderRequest {
                provider: "claude".parse().unwrap(),
                executable: args[4].clone().into(),
                working_directory: root.clone(),
                delegation: None,
                selection: None,
                mcp_servers: vec![],
                questions: false,
            },
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        let done = loop {
            if let Some(done) = pool.finished().pop() {
                break done;
            }
            if Instant::now() >= deadline {
                pool.stop(thread);
                panic!("synthetic provider turn timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(done.retired, "provider retirement was not confirmed");
        assert!(
            done.result.is_ok(),
            "session failed with a content-free code: {:?}",
            done.result
        );
        assert_eq!(pool.occupied(), 0);
        assert!(boundary.replies.load(Ordering::SeqCst) > before);
        let offers = boundary.offers.lock().unwrap();
        if pass == 0 {
            assert_eq!(offers[0].work.len(), 1);
        } else {
            assert!(
                offers[1].work.is_empty(),
                "exact replay must carry work without resending"
            );
            assert_eq!(offers[1].resume_accepted, vec![offers[0].id]);
        }
        println!(
            "PASS synthetic provider turn {} and confirmed process retirement",
            pass + 1
        );
    }
    assert_eq!(boundary.issued.load(Ordering::SeqCst), 2);
    println!(
        "PASS persisted native resume and exact input proof through the worker session driver"
    );
}
