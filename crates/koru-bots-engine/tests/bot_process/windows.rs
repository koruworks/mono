// SPDX-License-Identifier: MPL-2.0
use koru_agent_worker::machine::RestartBudget;
use koru_agent_worker::supervisor::{self, LaunchPlan, WorkerHandle, WorkerSpec};
use koru_bots_engine::{
    bot_worker,
    launcher::Policy,
    worker::{self, Bootstrap, Command, Reply},
};
use koru_session_runtime::providers::{AgentControl, AgentStart, CodexConfig, CodexControl};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

const ADDRESS: &str = "BOTS_REAL_WORKER_FIXTURE_ADDRESS";
const CASE: &str = "BOTS_REAL_WORKER_FIXTURE_CASE";
const BOT: Uuid = Uuid::from_u128(42);
const DEADLINE: Duration = Duration::from_secs(20);

pub fn run() {
    match std::env::args().nth(1).as_deref() {
        Some("app-server") => provider(),
        Some("fixture-leaf") => {
            report(4);
            park();
        }
        _ => {}
    }
    koru_agent_worker::dispatch(
        std::env::args(),
        &[
            koru_agent_worker::Role {
                name: "fixture-agent",
                run: agent,
            },
            koru_agent_worker::Role {
                name: "bots",
                run: engine,
            },
            koru_agent_worker::Role {
                name: "bot",
                run: bot,
            },
        ],
    );
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        println!("SKIP Bot process proof: requires --run --root");
        return;
    }
    assert!(koru_bots_engine::ENABLED);
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "--run");
    assert_eq!(args[1], "--root");
    let root = PathBuf::from(&args[2]);
    assert!(root.is_absolute());
    std::fs::create_dir(&root).expect("new disposable root");
    for case in [
        "idle",
        "unregistered",
        "wake-race",
        "kill-agent",
        "kill-engine",
        "kill-bot",
    ] {
        scenario(&root.join(case), case);
        println!("PASS {case}");
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct AgentBootstrap {
    engine: Bootstrap,
    address: String,
    case: String,
}
fn agent(_: Vec<String>) -> ! {
    let bytes = koru_agent_worker::read_bootstrap_line(&mut std::io::stdin()).unwrap();
    let config: AgentBootstrap = serde_json::from_slice(&bytes).unwrap();
    // This fresh fixture process has no threads yet; only its children inherit.
    unsafe {
        std::env::set_var(ADDRESS, &config.address);
        std::env::set_var(CASE, &config.case);
    }
    let child = config.engine.clone();
    let mut spec = WorkerSpec::new(
        "bots",
        move |_| {
            Ok(LaunchPlan {
                args: vec!["--root".into(), child.root.clone().into_os_string()],
                bootstrap: serde_json::to_vec(&child).unwrap(),
                payload: child.clone(),
            })
        },
        |b: &Bootstrap| {
            let ep = worker::endpoint(&b.root, &b.segment).unwrap();
            match worker::request(&ep, &b.authority, Command::Hello) {
                Ok(Reply::Hello { launch_id }) if launch_id == b.launch_id => Ok(()),
                _ => Err("not ready".into()),
            }
        },
    );
    spec.restart_budget = RestartBudget {
        max_restarts: 0,
        window: DEADLINE,
    };
    spec.stop_deadline = Duration::from_secs(2);
    let handle = supervisor::start(spec);
    report(0);
    std::mem::forget(handle);
    park();
}
fn engine(args: Vec<String>) -> ! {
    let [flag, root] = args.as_slice() else {
        std::process::exit(2)
    };
    assert_eq!(flag, "--root");
    report(1);
    let case = std::env::var(CASE).unwrap();
    let policy = Policy {
        registration_timeout: Duration::from_secs(2),
        idle_window: if case == "idle" {
            Duration::from_millis(800)
        } else {
            Duration::from_secs(60)
        },
    };
    let result = worker::run_supervised_with_policy(Path::new(root), policy);
    if let Err(error) = &result {
        eprintln!("engine: {error}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
fn bot(args: Vec<String>) -> ! {
    assert_eq!(args, vec!["--bot", &BOT.to_string()]);
    let case = std::env::var(CASE).unwrap();
    report(2);
    if case == "unregistered" {
        park();
    }
    let _provider = if case.starts_with("kill-") {
        let config = CodexConfig::new(
            std::env::current_exe().unwrap().into_os_string(),
            std::env::current_dir().unwrap(),
            "0.169.0".parse().unwrap(),
            Arc::new(koru_session_runtime::NoopAttachmentPathResolver),
        )
        .unwrap();
        let mut provider = CodexControl::new(config);
        provider
            .start(AgentStart::fresh(
                Uuid::new_v4().to_string().parse().unwrap(),
                None,
            ))
            .unwrap();
        Some(provider)
    } else {
        None
    };
    let result = bot_worker::run_supervised(BOT);
    if let Err(error) = &result {
        eprintln!("Bot: {error}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
fn report(level: u8) {
    let mut stream = TcpStream::connect(std::env::var(ADDRESS).unwrap()).unwrap();
    stream.set_write_timeout(Some(DEADLINE)).unwrap();
    writeln!(stream, "{level} {}", std::process::id()).unwrap();
}
fn park() -> ! {
    loop {
        std::thread::park();
    }
}
fn provider() -> ! {
    let leaf = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("fixture-leaf")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::mem::forget(leaf);
    report(3);
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                json!({"userAgent":"koru_terminal/0.169.0","codexHome":"fixture","platformFamily":"windows","platformOs":"windows"})
            }
            Some("model/list") => {
                json!({"data":[{"id":"fixture","model":"fixture","isDefault":true,"inputModalities":["text"],"displayName":"Fixture","supportedReasoningEfforts":[{"reasoningEffort":"medium"}],"defaultReasoningEffort":"medium"}],"nextCursor":null})
            }
            Some("thread/start") => json!({"thread":{"id":"fixture-thread"}}),
            other => panic!("unexpected method {other:?}"),
        };
        println!("{}", json!({"id":id,"result":result}));
        std::io::stdout().flush().unwrap();
    }
    park();
}

struct Process(HANDLE);
impl Process {
    fn open(pid: u32) -> Self {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        assert!(!handle.is_null());
        Self(handle)
    }
    fn kill(&self) {
        assert_ne!(unsafe { TerminateProcess(self.0, 91) }, 0);
    }
    fn state(&self, timeout: u32) -> u32 {
        unsafe { WaitForSingleObject(self.0, timeout) }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn scenario(root: &Path, case: &str) {
    std::fs::create_dir(root).unwrap();
    std::fs::create_dir(root.join("settings")).unwrap();
    std::fs::write(
        root.join("settings/desktop-v1.json"),
        br#"{"bots_session_capacity":2}"#,
    )
    .unwrap();
    drop(koru_bots_engine::Database::open(root).unwrap());
    let db = rusqlite::Connection::open(root.join("bots/engine.db")).unwrap();
    db.execute("INSERT INTO bots(id,owner,name,settings,access,knowledge_pages,visibility) VALUES (?1,'fixture','fixture',x'7b7d',x'7b7d',x'5b5d','private')",[BOT.to_string()]).unwrap();
    drop(db);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = AgentBootstrap {
        engine: Bootstrap {
            launch_id: Uuid::new_v4(),
            root: root.into(),
            segment: koru_channel::segment().into(),
            authority: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        },
        address: listener.local_addr().unwrap().to_string(),
        case: case.into(),
    };
    let child = config.clone();
    let mut spec = WorkerSpec::new(
        "fixture-agent",
        move |_| {
            Ok(LaunchPlan {
                args: vec![],
                bootstrap: serde_json::to_vec(&child).unwrap(),
                payload: (),
            })
        },
        |_| Ok(()),
    );
    spec.restart_budget = RestartBudget {
        max_restarts: 0,
        window: DEADLINE,
    };
    spec.stop_deadline = Duration::from_secs(2);
    let handle: WorkerHandle<()> = supervisor::start(spec);
    let result = std::panic::catch_unwind(|| {
        let ep = worker::endpoint(root, koru_channel::segment()).unwrap();
        let deadline = Instant::now() + DEADLINE;
        loop {
            if matches!(
                worker::request(&ep, &config.engine.authority, Command::Hello),
                Ok(Reply::Hello { .. })
            ) {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(matches!(
            worker::request(&ep, &config.engine.authority, Command::WakeBot { bot: BOT }),
            Ok(Reply::Queued)
        ));
        assert!(
            matches!(
                worker::request(&ep, &config.engine.authority, Command::Capacity),
                Ok(Reply::Capacity { sessions: 2 })
            ),
            "saved host capacity is read by the launcher"
        );
        // A repeated wake shares the one Bot-keyed launcher entry.
        assert!(matches!(
            worker::request(&ep, &config.engine.authority, Command::WakeBot { bot: BOT }),
            Ok(Reply::Queued)
        ));
        let count = if case.starts_with("kill-") { 5 } else { 3 };
        let mut processes = BTreeMap::new();
        while processes.len() < count {
            assert!(Instant::now() < deadline, "process chain missing");
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut line = String::new();
                    BufReader::new(stream).read_line(&mut line).unwrap();
                    let (level, pid) = line.trim().split_once(' ').unwrap();
                    let level: u8 = level.parse().unwrap();
                    assert!(
                        processes
                            .insert(level, Process::open(pid.parse().unwrap()))
                            .is_none(),
                        "duplicate Bot worker"
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("{e}"),
            }
        }
        if case == "unregistered" {
            assert_eq!(
                processes[&2].state(6000),
                WAIT_OBJECT_0,
                "registration timeout must stop the worker"
            );
        } else {
            loop {
                if matches!(worker::request(&ep,&config.engine.authority,Command::Status),Ok(Reply::Status(s))if s.awake_bots==1)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "registration missing");
                std::thread::sleep(Duration::from_millis(10));
            }
            if case == "wake-race" {
                let old = match worker::request(&ep, &config.engine.authority, Command::Status)
                    .unwrap()
                {
                    Reply::Status(s) => s.workers[0].incarnation.clone(),
                    _ => panic!("status"),
                };
                assert!(matches!(
                    worker::request(&ep, &config.engine.authority, Command::StopBot { bot: BOT }),
                    Ok(Reply::Queued)
                ));
                assert!(matches!(
                    worker::request(&ep, &config.engine.authority, Command::WakeBot { bot: BOT }),
                    Ok(Reply::Queued)
                ));
                loop {
                    if matches!(worker::request(&ep,&config.engine.authority,Command::Status),Ok(Reply::Status(s)) if s.workers.first().is_some_and(|w|w.incarnation!=old))
                    {
                        break;
                    }
                    assert!(Instant::now() < deadline, "wake during stop was lost");
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(
                    processes[&2].state(0),
                    WAIT_OBJECT_0,
                    "replacement cannot overlap the stopped incarnation"
                );
            } else if case == "idle" {
                assert_eq!(
                    processes[&2].state(6000),
                    WAIT_OBJECT_0,
                    "idle worker must exit"
                );
                assert!(
                    root.join("bots")
                        .join(BOT.to_string())
                        .join("bot.db")
                        .is_file()
                );
            } else {
                let killed = match case {
                    "kill-agent" => 0,
                    "kill-engine" => 1,
                    _ => 2,
                };
                processes[&killed].kill();
                for (&level, process) in &processes {
                    assert_eq!(
                        process.state(if level >= killed { 5000 } else { 0 }),
                        if level >= killed {
                            WAIT_OBJECT_0
                        } else {
                            WAIT_TIMEOUT
                        },
                        "liveness of level {level}"
                    );
                }
            }
        }
    });
    handle.stop();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
