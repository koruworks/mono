// SPDX-License-Identifier: MPL-2.0

//! Root-qualified private service; each bounded connection runs Store work off
//! the reactor. Shutdown closes admission and drains calls before Store joins.
use koru_api_gateway_core::{
    GatewayState,
    bots_workspace::{Boundary, Engine},
};
use koru_bots_engine::{authority::Secret, workspace};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

pub(crate) struct Server {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}
impl Server {
    pub(crate) async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}
pub(crate) fn start(
    config: &crate::GatewayConfig,
    state: &GatewayState,
) -> Result<Option<Server>, String> {
    if !koru_bots_engine::ENABLED {
        return Ok(None);
    }
    let Some(authority) = config
        .bootstrap
        .as_ref()
        .and_then(|b| b.bots_engine.as_ref())
    else {
        // A standalone gateway cannot manufacture an Agent's engine authority.
        return Ok(None);
    };
    let engine = Engine {
        endpoint: koru_bots_engine::worker::endpoint(&config.channel_root, &config.build.segment)
            .map_err(|e| e.to_string())?,
        authority: Secret::parse(authority.expose().to_owned()).map_err(|e| e.to_string())?,
    };
    let device = config
        .bootstrap
        .as_ref()
        .and_then(|b| b.device_id.as_deref())
        .ok_or_else(|| "engine_owner_unavailable".to_owned())?;
    if !state.set_bots_engine(engine.clone(), device) {
        return Err("engine_owner_unavailable".into());
    }
    let boundary = Arc::new(state.bots_workspace(engine));
    let endpoint = workspace::endpoint(&config.channel_root, &config.build.segment)
        .map_err(|e| e.to_string())?;
    let listener = koru_ipc::AsyncListener::bind(endpoint)
        .map_err(|_| "engine_workspace_unavailable".to_owned())?;
    let (stop, mut stopping) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut calls = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                _ = calls.join_next(), if !calls.is_empty() => {},
                stream = listener.accept() => {
                    let Ok(stream) = stream else { break };
                    if calls.len() >= 8 || !koru_ipc::same_user(&stream) { continue; }
                    calls.spawn(handle(stream, boundary.clone()));
                }
            }
        }
        drop(listener);
        while calls.join_next().await.is_some() {}
    });
    Ok(Some(Server { stop, task }))
}
async fn handle(mut stream: koru_ipc::AsyncStream, boundary: Arc<Boundary>) {
    let read = async {
        let mut header = [0; koru_ipc::frame::HEADER_LEN];
        stream.read_exact(&mut header).await.ok()?;
        let header = koru_ipc::frame::parse_header(&header).ok()?;
        if header.version != workspace::VERSION {
            return None;
        }
        let mut bytes = vec![0; header.body_len];
        stream.read_exact(&mut bytes).await.ok()?;
        koru_ipc::frame::decode::<workspace::Command>(&bytes).ok()
    };
    let Ok(Some(request)) = tokio::time::timeout(workspace::TIMEOUT, read).await else {
        return;
    };
    // Do not abandon a dispatched operation on client disconnect or timeout.
    // Its bounded admission slot remains held until the owner finishes.
    let Ok(reply) = tokio::task::spawn_blocking(move || boundary.handle(request)).await else {
        return;
    };
    let Ok(bytes) = koru_ipc::frame::encode(workspace::VERSION, &reply) else {
        return;
    };
    let _ = tokio::time::timeout(workspace::TIMEOUT, stream.write_all(&bytes)).await;
}
