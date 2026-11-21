// SPDX-License-Identifier: MPL-2.0

//! Explicit process/disk proof. Ordinary cargo test prints SKIP. Invocation:
//! cargo test -p koru-bots-engine --test engine_process --
//!   --run --root <new-absolute-disposable-directory>

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use koru_agent_worker::machine::{RestartBudget, State};
use koru_agent_worker::supervisor::{self, LaunchPlan, WorkerHandle, WorkerSpec};
use koru_bots_engine::worker::{self, Bootstrap, Command, Reply};

fn role(args: Vec<String>) -> ! {
    let [flag, root] = args.as_slice() else {
        std::process::exit(2)
    };
    if flag != "--root" {
        std::process::exit(2);
    }
    let result = worker::run_supervised(Path::new(root));
    std::process::exit(if result.is_ok() { 0 } else { 1 })
}

fn main() {
    koru_agent_worker::dispatch(
        std::env::args(),
        &[koru_agent_worker::Role {
            name: "bots",
            run: role,
        }],
    );
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_none_or(|arg| arg.as_str() != "--run") {
        println!("SKIP engine process proof: requires --run --root");
        return;
    }
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "--run");
    assert_eq!(args[1], "--root");
    let root = PathBuf::from(&args[2]);
    assert!(root.is_absolute());
    // create_dir, not create_dir_all: never reuse an existing root.
    std::fs::create_dir(&root).expect("new disposable root");
    let (handle, bootstrap) = launch(&root);
    wait(&handle, State::Ready);
    let endpoint = worker::endpoint(&root, koru_channel::segment()).unwrap();
    assert!(
        matches!(worker::request(&endpoint, &bootstrap.authority, Command::Status), Ok(Reply::Status(status)) if status == Default::default())
    );
    assert!(matches!(
        worker::request(&endpoint, "wrong-authority", Command::Stop),
        Ok(Reply::Refused(koru_bots_engine::Error::Unauthenticated))
    ));
    assert!(matches!(
        worker::request(&endpoint, &bootstrap.authority, Command::Hello),
        Ok(Reply::Hello { .. })
    ));
    assert!(root.join("bots/engine-owned").is_file());
    handle.stop();
    assert_eq!(koru_bots_engine::inspect(&root).unwrap().schema_version, 3);
    println!("PASS engine starts, authenticates control, answers status and marks root");

    prove_interrupted_recovery(&root);

    let connection = rusqlite::Connection::open(root.join("bots/engine.db")).unwrap();
    connection.execute_batch("PRAGMA user_version=4;").unwrap();
    drop(connection);
    let (newer, _) = launch(&root);
    wait(&newer, State::Exhausted);
    assert!(newer.current().is_none());
    newer.stop();
    let report = koru_bots_engine::inspect(&root).unwrap();
    assert_eq!(report.schema_version, 4);
    assert_eq!(
        report.status_error,
        Some(koru_bots_engine::Error::SchemaTooNew)
    );
    println!("PASS newer schema refuses startup; read-only diagnostic still answers");
}

fn prove_interrupted_recovery(root: &Path) {
    use koru_bots_engine::{
        Database,
        messages::{Envelope, Person, Resume},
        person,
    };
    use uuid::Uuid;
    let bot = Uuid::new_v4();
    let thread = Uuid::new_v4();
    let seed = rusqlite::Connection::open(root.join("bots/engine.db")).unwrap();
    seed.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'fixture','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[bot.to_string()]).unwrap();
    seed.execute(
        "INSERT INTO threads(id,bot,kind,title,lifecycle) VALUES (?1,?2,'main','','active')",
        rusqlite::params![thread.to_string(), bot.to_string()],
    )
    .unwrap();
    seed.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,'fixture','person'),(?1,?2,'bot')",rusqlite::params![thread.to_string(),bot.to_string()]).unwrap();
    drop(seed);
    let mut db = Database::open(root).unwrap();
    let person = Person {
        sender: "fixture".into(),
        owner: "fixture".into(),
    };
    let set_team_pause = |db: &mut Database, paused: bool, revision: u64, now: i64| {
        let reply = person::handle(
            db,
            &Default::default(),
            &mut Default::default(),
            person::Request {
                owner: person.owner.clone(),
                sender: person.sender.clone(),
                command: person::Command::Change {
                    id: Uuid::new_v4(),
                    action: person::Change::Team { revision, paused },
                },
            },
            now,
        )
        .unwrap();
        let receipt: koru_bots_engine::messages::Receipt<person::Team> =
            serde_json::from_value(reply.0).unwrap();
        assert_eq!(receipt.answer.paused, paused);
        assert_eq!(receipt.answer.revision, revision + 1);
    };
    let envelope = Envelope {
        id: Uuid::new_v4(),
        to: bot,
        thread: Some(thread),
        in_reply_to: None,
        body: "synthetic unfinished work".into(),
        attachments: vec![],
        references: vec![],
        deadline_ms: None,
        reply_to: koru_bots_engine::messages::ReplyTo::Thread,
    };
    let accepted = db.accept_message(&person, &envelope, 1).unwrap().answer;
    db.claim_messages(
        bot,
        thread,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Resume::Rebuilt,
        8192,
        2,
    )
    .unwrap()
    .unwrap();
    // Recovery must restore the durable work without admitting a fresh Bot
    // worker: this executable intentionally registers only the engine role.
    // Team pause leaves the running turn intact for startup to interrupt.
    set_team_pause(&mut db, true, 0, 3);
    drop(db);
    let (handle, bootstrap) = launch(root);
    wait(&handle, State::Ready);
    let endpoint = worker::endpoint(root, koru_channel::segment()).unwrap();
    assert!(
        matches!(worker::request(&endpoint,&bootstrap.authority,Command::Status),Ok(Reply::Status(s)) if s.running_turns==0&&s.inbox_depth==1)
    );
    handle.stop();
    let mut db = Database::open(root).unwrap();
    // Resume through the same revision-checked contract only after the engine
    // process has stopped, so the following manual claim cannot race it.
    set_team_pause(&mut db, false, 1, 4);
    let continued = db
        .claim_messages(
            bot,
            thread,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Resume::Rebuilt,
            8192,
            5,
        )
        .unwrap()
        .unwrap();
    assert!(continued.continuation && continued.uncertain);
    assert_eq!(continued.work[0].id, accepted.message);
    println!("PASS engine startup interrupts durable running work and retains it for continuation");
}

fn launch(root: &Path) -> (WorkerHandle<Bootstrap>, Bootstrap) {
    let bootstrap = Bootstrap {
        launch_id: uuid::Uuid::new_v4(),
        segment: koru_channel::segment().into(),
        root: root.into(),
        authority: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
    };
    let child = bootstrap.clone();
    let mut spec = WorkerSpec::new(
        "bots",
        move |_| {
            Ok(LaunchPlan {
                args: vec!["--root".into(), child.root.clone().into_os_string()],
                bootstrap: serde_json::to_vec(&child).unwrap(),
                payload: child.clone(),
            })
        },
        |child: &Bootstrap| {
            let endpoint =
                worker::endpoint(&child.root, &child.segment).map_err(|e| e.to_string())?;
            match worker::request(&endpoint, &child.authority, Command::Hello) {
                Ok(Reply::Hello { launch_id }) if launch_id == child.launch_id => Ok(()),
                _ => Err("not ready".into()),
            }
        },
    );
    spec.restart_budget = RestartBudget {
        max_restarts: 0,
        window: Duration::from_secs(60),
    };
    spec.ready_deadline = Duration::from_secs(10);
    spec.stop_deadline = Duration::from_secs(2);
    spec.graceful_stop = Some(Box::new(|child| {
        let endpoint = worker::endpoint(&child.root, &child.segment).map_err(|e| e.to_string())?;
        worker::request(&endpoint, &child.authority, Command::Stop)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }));
    (supervisor::start(spec), bootstrap)
}

fn wait(handle: &WorkerHandle<Bootstrap>, expected: State) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if handle.status().state == expected {
            return;
        }
        if Instant::now() >= deadline {
            handle.stop();
            panic!("worker did not reach {expected:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
