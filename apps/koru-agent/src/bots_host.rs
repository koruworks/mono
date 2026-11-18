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

pub fn start(paths: &Paths) -> Result<(), koru_bots_engine::Error> {
    if !koru_bots_engine::ENABLED {
        return Ok(());
    }
    let root = paths.root().to_path_buf();
    let segment = crate::segment().to_owned();
    let endpoint = worker::endpoint(&root, &segment)?;
    let mut spec = WorkerSpec::new(
        "bots",
        move |_| {
            let bootstrap = Bootstrap {
                launch_id: uuid::Uuid::new_v4(),
                segment: segment.clone(),
                root: root.clone(),
                authority: format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                ),
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
