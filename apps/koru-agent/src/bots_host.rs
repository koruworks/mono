// SPDX-License-Identifier: MPL-2.0

//! The Agent's asynchronous supervision adapter for the Bots engine. Shared
//! startup composes it for both tray and headless server operation.

use std::sync::OnceLock;
use std::time::Duration;

use koru_agent_worker::supervisor::{self, LaunchPlan, WorkerHandle, WorkerSpec};
use koru_bots_engine::worker::{self, Bootstrap, Command, Reply};

use crate::Paths;

struct Child {
    bootstrap: Bootstrap,
    endpoint: koru_ipc::Endpoint,
}

static BOTS: OnceLock<WorkerHandle<Child>> = OnceLock::new();
static AUTHORITY: OnceLock<String> = OnceLock::new();
static WAKES: OnceLock<std::sync::mpsc::SyncSender<()>> = OnceLock::new();

/// Native callbacks enqueue one edge; IPC/retry never runs on the window thread.
pub fn wake() {
    if let Some(sender) = WAKES.get() {
        let _ = sender.try_send(());
    }
}

/// Per-Agent lifetime, shared privately with its gateway across child restarts.
/// A provider or Bot-worker bootstrap never receives this capability.
pub fn authority() -> String {
    AUTHORITY
        .get_or_init(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        })
        .clone()
}

pub fn start(paths: &Paths) -> Result<(), koru_bots_engine::Error> {
    let root = paths.root().to_path_buf();
    let segment = crate::segment().to_owned();
    let endpoint = worker::endpoint(&root, &segment)?;
    let authority = authority();
    let (wake_sender, wake_receiver) = std::sync::mpsc::sync_channel(1);
    let wake_endpoint = endpoint.clone();
    let wake_authority = authority.clone();
    std::thread::Builder::new()
        .name("koru-bots wakes".into())
        .spawn(move || {
            while wake_receiver.recv().is_ok() {
                loop {
                    if matches!(
                        worker::request(&wake_endpoint, &wake_authority, Command::ClockChanged),
                        Ok(Reply::Queued)
                    ) {
                        break;
                    }
                    // A pending native edge survives a supervised restart. All
                    // repeated native events coalesce into this bounded ingress.
                    match wake_receiver.recv_timeout(Duration::from_secs(1)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        _ => {}
                    }
                }
            }
        })
        .map_err(|_| koru_bots_engine::Error::EngineUnavailable)?;
    let _ = WAKES.set(wake_sender);
    let mut spec = WorkerSpec::new(
        "bots",
        move |_| {
            let bootstrap = Bootstrap {
                launch_id: uuid::Uuid::new_v4(),
                segment: segment.clone(),
                root: root.clone(),
                authority: authority.clone(),
            };
            Ok(LaunchPlan {
                args: vec!["--root".into(), root.clone().into_os_string()],
                bootstrap: serde_json::to_vec(&bootstrap)
                    .map_err(|_| "invalid_request".to_owned())?,
                payload: Child {
                    bootstrap,
                    endpoint: endpoint.clone(),
                },
            })
        },
        |child: &Child| hello(child),
    );
    spec.ready_deadline = Duration::from_secs(30);
    spec.stop_deadline = Duration::from_secs(5);
    spec.health_interval = Duration::from_secs(30);
    spec.health = Some(Box::new(hello));
    spec.graceful_stop = Some(Box::new(|child| {
        worker::request(&child.endpoint, &child.bootstrap.authority, Command::Stop)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }));
    let _ = BOTS.set(supervisor::start(spec));
    Ok(())
}

fn hello(child: &Child) -> Result<(), String> {
    match worker::request(&child.endpoint, &child.bootstrap.authority, Command::Hello) {
        Ok(Reply::Hello { launch_id }) if launch_id == child.bootstrap.launch_id => Ok(()),
        _ => Err("engine_unavailable".into()),
    }
}

pub fn stop() {
    if let Some(supervisor) = BOTS.get() {
        supervisor.stop();
    }
}

pub fn status() -> Option<koru_agent_worker::supervisor::StatusReport> {
    BOTS.get().map(WorkerHandle::status)
}
