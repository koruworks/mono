// SPDX-License-Identifier: MPL-2.0

//! One loopback endpoint per Bot worker. Session installation and revocation
//! are local lifecycle operations; all authorization still reaches the engine.
use super::*;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use rmcp::model::{Implementation, InitializeResult, ProtocolVersion, ServerCapabilities};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Mutex};
use tokio::sync::{Semaphore, oneshot};

#[derive(Default)]
pub struct Registry(Mutex<BTreeMap<[u8; 32], Arc<Door>>>);
impl Registry {
    pub fn install(&self, door: Door) -> Result<(), Error> {
        let key = Sha256::digest(door.secret.expose().as_bytes()).into();
        let mut doors = self.0.lock().map_err(|_| Error::EngineUnavailable)?;
        if doors.contains_key(&key) {
            return Err(Error::Conflict);
        }
        doors.insert(key, Arc::new(door));
        Ok(())
    }
    pub fn remove(&self, secret: &Secret) {
        if let Ok(mut doors) = self.0.lock() {
            let key: [u8; 32] = Sha256::digest(secret.expose().as_bytes()).into();
            doors.remove(&key);
        }
    }
    fn resolve(&self, authorization: &str) -> Result<Arc<Door>, Error> {
        let secret = authorization
            .strip_prefix("Bearer ")
            .ok_or(Error::Unauthenticated)?;
        let secret = Secret::parse(secret.to_owned())?;
        let key: [u8; 32] = Sha256::digest(secret.expose().as_bytes()).into();
        self.0
            .lock()
            .map_err(|_| Error::EngineUnavailable)?
            .get(&key)
            .cloned()
            .ok_or(Error::Unauthenticated)
    }
}
struct HttpState {
    registry: Arc<Registry>,
    host: String,
    calls: Arc<Semaphore>,
}
pub struct Server {
    endpoint: koru_session_protocol::McpEndpoint,
    stop: Option<oneshot::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    /// Called on worker startup, not on its heartbeat or a provider callback.
    pub fn start(registry: Arc<Registry>) -> Result<Self, Error> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| Error::EngineUnavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Error::EngineUnavailable)?;
        let host = listener
            .local_addr()
            .map_err(|_| Error::EngineUnavailable)?
            .to_string();
        let endpoint = format!("http://{host}/mcp")
            .parse()
            .map_err(|_| Error::InvalidRequest)?;
        let (stop, stopping) = oneshot::channel();
        let (ready, answer) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("koru-bot tools".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        return;
                    };
                    let router = Router::new()
                        .route("/mcp", post(handle_request))
                        .layer(DefaultBodyLimit::max(crate::limits::MAX_PAYLOAD_BYTES))
                        .with_state(Arc::new(HttpState {
                            registry,
                            host,
                            calls: Arc::new(Semaphore::new(8)),
                        }));
                    let _ = ready.send(());
                    let _ = axum::serve(listener, router)
                        .with_graceful_shutdown(async {
                            let _ = stopping.await;
                        })
                        .await;
                });
            })
            .map_err(|_| Error::EngineUnavailable)?;
        answer
            .recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| Error::EngineUnavailable)?;
        Ok(Self {
            endpoint,
            stop: Some(stop),
            handle: Some(handle),
        })
    }
    pub fn endpoint(&self) -> koru_session_protocol::McpEndpoint {
        self.endpoint.clone()
    }
    /// Stop admission; already bound calls drain under their original identity.
    pub fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
async fn handle_request(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let headers = request.headers();
    // A browser origin is never an MCP provider. Exact Host also refuses DNS
    // rebinding; no CORS surface is opened for this loopback capability.
    if headers.contains_key("origin")
        || headers.get("host").and_then(|h| h.to_str().ok()) != Some(state.host.as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(authorization) = headers.get("authorization").and_then(|h| h.to_str().ok()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(door) = state.registry.resolve(authorization) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(permit) = state.calls.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(Ok(bytes)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        axum::body::to_bytes(request.into_body(), crate::limits::MAX_PAYLOAD_BYTES),
    )
    .await
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        rpc(&door, &bytes)
    })
    .await
    {
        Ok(Ok(Some(value))) => Json(value).into_response(),
        Ok(Ok(None)) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(Error::Unauthenticated | Error::CredentialRevoked | Error::WorkerReplaced)) => {
            StatusCode::UNAUTHORIZED.into_response()
        }
        Ok(Err(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn rpc(door: &Door, bytes: &[u8]) -> Result<Option<Value>, Error> {
    if bytes.len() > crate::limits::MAX_PAYLOAD_BYTES {
        return Err(Error::InvalidRequest);
    }
    // Reauthenticate even initialize/ping/notifications. Static discovery never
    // becomes a stale bearer cache, and duplicate keys are rejected at any depth.
    let tools = door.catalog()?;
    let Unique(packet) = serde_json::from_slice(bytes).map_err(|_| Error::InvalidRequest)?;
    let packet = packet.as_object().ok_or(Error::InvalidRequest)?;
    if packet.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::InvalidRequest);
    }
    let method = packet
        .get("method")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidRequest)?;
    let id = packet.get("id");
    if method == "notifications/initialized" && id.is_none() {
        return Ok(None);
    }
    let id = id
        .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
        .ok_or(Error::InvalidRequest)?;
    let params = packet
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let result = match method {
        "initialize" => {
            let offered: ProtocolVersion = serde_json::from_value(
                params
                    .get("protocolVersion")
                    .cloned()
                    .ok_or(Error::InvalidRequest)?,
            )
            .map_err(|_| Error::InvalidRequest)?;
            let version = if ProtocolVersion::KNOWN_VERSIONS.contains(&offered) {
                offered
            } else {
                ProtocolVersion::LATEST
            };
            serde_json::to_value(
                InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
                    .with_protocol_version(version)
                    .with_server_info(Implementation::new("koru-bot", env!("CARGO_PKG_VERSION"))),
            )
            .map_err(|_| Error::InvalidRequest)?
        }
        "ping" => serde_json::json!({}),
        "tools/list" => serde_json::json!({"tools":tools}),
        "tools/call" => {
            let raw = serde_json::to_vec(&params).map_err(|_| Error::InvalidRequest)?;
            match door.call(&raw) {
                Ok(value) => {
                    serde_json::json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
                }
                Err(error) => {
                    serde_json::json!({"content":[{"type":"text","text":error.to_string()}],"structuredContent":{"code":error.to_string()},"isError":true})
                }
            }
        }
        _ => {
            return Ok(Some(
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method_not_found"}}),
            ));
        }
    };
    Ok(Some(
        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
    ))
}

#[cfg(test)]
mod tests;
