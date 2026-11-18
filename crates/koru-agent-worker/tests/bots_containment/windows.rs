// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use koru_agent_worker::machine::RestartBudget;
use koru_agent_worker::supervisor::{self, LaunchPlan, WorkerHandle, WorkerSpec};
use koru_session_runtime::NoopAttachmentPathResolver;
use koru_session_runtime::providers::{AgentControl, AgentStart, CodexConfig, CodexControl};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

const DEADLINE: Duration = Duration::from_secs(15);
const ADDRESS_ENV: &str = "BOTS_CONTAINMENT_FIXTURE_ADDRESS";

pub fn run() {
    match std::env::args().nth(1).as_deref() {
        Some("fixture-leaf") => {
            report(4);
            park();
        }
        Some("app-server") => provider(),
        _ => {}
    }
    koru_agent_worker::dispatch(
        std::env::args(),
        &[koru_agent_worker::Role {
            name: "chain",
            run: worker,
        }],
    );
    for (level, name) in [(0, "Agent"), (1, "engine"), (2, "Bot worker")] {
        match scenario(level) {
            Ok(()) => println!("PASS kill {name}: descendants ended, ancestors survived"),
            Err(error) => {
                eprintln!("FAIL kill {name}: {error}");
                std::process::exit(1);
            }
        }
    }
}

fn start(level: u8, address: SocketAddr) -> WorkerHandle<()> {
    let mut spec = WorkerSpec::new(
        "chain",
        move |_| {
            Ok(LaunchPlan {
                args: vec![],
                bootstrap: format!("{level} {address}").into_bytes(),
                payload: (),
            })
        },
        |_| Ok(()),
    );
    // A dead descendant must not be replaced while we assert the old chain.
    spec.restart_budget = RestartBudget {
        max_restarts: 0,
        window: Duration::from_secs(60),
    };
    spec.ready_deadline = DEADLINE;
    spec.stop_deadline = Duration::from_secs(2);
    supervisor::start(spec)
}

fn worker(_: Vec<String>) -> ! {
    let line = koru_agent_worker::read_bootstrap_line(&mut std::io::stdin()).expect("bootstrap");
    let text = std::str::from_utf8(&line).expect("bootstrap UTF-8");
    let (level, address) = text.trim().split_once(' ').expect("bootstrap fields");
    let level: u8 = level.parse().expect("level");
    let address: SocketAddr = address.parse().expect("loopback address");
    // This fresh test role has not created threads yet. Only its descendants
    // inherit this fixture endpoint; the suite process never changes its env.
    unsafe { std::env::set_var(ADDRESS_ENV, address.to_string()) };
    if level < 2 {
        let child = start(level + 1, address);
        let deadline = Instant::now() + DEADLINE;
        while child.current().is_none() {
            assert!(Instant::now() < deadline, "child was not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        report(level);
        // Intentional abrupt lifetime. The test terminates this process and
        // must not rely on WorkerHandle::drop or graceful-stop callbacks.
        std::mem::forget(child);
        park();
    }
    assert_eq!(level, 2);
    let config = CodexConfig::new(
        std::env::current_exe()
            .expect("fixture executable")
            .into_os_string(),
        std::env::current_dir().expect("cwd"),
        "0.169.0".parse().expect("fixture version"),
        Arc::new(NoopAttachmentPathResolver),
    )
    .expect("provider config");
    let mut agent = CodexControl::new(config);
    agent
        .start(AgentStart::fresh(
            "00000000-0000-4000-8000-000000000003"
                .parse()
                .expect("fixture session"),
            None,
        ))
        .expect("stand-in provider handshake");
    report(2);
    // The real provider transport owns its job; no destructor gets to help.
    std::mem::forget(agent);
    park();
}

fn report(level: u8) {
    let address = std::env::var(ADDRESS_ENV).expect("fixture endpoint");
    let mut stream = TcpStream::connect(address).expect("report connection");
    stream
        .set_write_timeout(Some(DEADLINE))
        .expect("write deadline");
    writeln!(stream, "{level} {}", std::process::id()).expect("report");
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

fn provider() -> ! {
    let leaf = Command::new(std::env::current_exe().expect("fixture executable"))
        .arg("fixture-leaf")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("provider descendant");
    std::mem::forget(leaf);
    report(3);
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.expect("frame")).expect("JSON frame");
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request.get("method").and_then(Value::as_str) {
            Some("initialize") => json!({
                "userAgent":"koru_terminal/0.169.0", "codexHome":"fixture",
                "platformFamily":"windows", "platformOs":"windows",
            }),
            Some("model/list") => json!({"data":[{
                "id":"fixture-model", "model":"fixture-model", "isDefault":true,
                "inputModalities":["text"], "displayName":"Fixture",
                "supportedReasoningEfforts":[{"reasoningEffort":"medium"}],
                "defaultReasoningEffort":"medium",
            }], "nextCursor":null}),
            Some("thread/start") => json!({"thread":{"id":"fixture-thread"}}),
            other => panic!("unexpected fixture method: {other:?}"),
        };
        println!("{}", json!({"id":id,"result":result}));
        std::io::stdout().flush().expect("reply flush");
    }
    // EOF does not make this stand-in cooperate with shutdown. Its job must
    // kill it and its indefinitely waiting child.
    park();
}

struct Process(HANDLE);

impl Process {
    fn open(pid: u32) -> Result<Self, String> {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "could not hold fixture process: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(handle))
    }

    fn terminate(&self) -> Result<(), String> {
        if unsafe { TerminateProcess(self.0, 91) } == 0 {
            return Err(format!(
                "terminate failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
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

fn scenario(killed: u8) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let root = start(0, listener.local_addr().map_err(|e| e.to_string())?);
    let result = (|| {
        let deadline = Instant::now() + DEADLINE;
        let mut processes = BTreeMap::new();
        while processes.len() != 5 {
            if Instant::now() >= deadline {
                return Err("whole chain did not become ready".into());
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .map_err(|e| e.to_string())?;
                    let mut line = String::new();
                    BufReader::new(stream)
                        .read_line(&mut line)
                        .map_err(|e| e.to_string())?;
                    let (level, pid) = line.trim().split_once(' ').ok_or("bad fixture report")?;
                    let level: u8 = level.parse().map_err(|_| "bad level")?;
                    let pid: u32 = pid.parse().map_err(|_| "bad pid")?;
                    if level > 4 || processes.contains_key(&level) {
                        return Err("duplicate/invalid level".into());
                    }
                    // Retain handles before killing anything: a reused PID
                    // cannot make either the death or survivor assertion pass.
                    processes.insert(level, Process::open(pid)?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        for process in processes.values() {
            if process.state(0) != WAIT_TIMEOUT {
                return Err("fixture died before the kill".into());
            }
        }
        processes[&killed].terminate()?;
        for (&level, process) in &processes {
            let expected = if level >= killed {
                WAIT_OBJECT_0
            } else {
                WAIT_TIMEOUT
            };
            let timeout = if level >= killed { 5_000 } else { 0 };
            if process.state(timeout) != expected {
                return Err(format!(
                    "wrong liveness after killing level {killed}: level {level}"
                ));
            }
        }
        Ok(())
    })();
    // Always stop only our own root, including on a failing readiness check.
    root.stop();
    result
}
