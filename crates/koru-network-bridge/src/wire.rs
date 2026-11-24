// SPDX-License-Identifier: MPL-2.0

//! Bounded binary multiplexing over the private Agent/gateway local socket.
//!
//! The registration uses the ordinary small JSON IPC frame. After it succeeds,
//! this module switches to a fixed binary header so opaque payload bytes are
//! neither JSON encoded nor copied into a second serialization format.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use koru_network_edge::{
    ConnectionEvent, ConnectionMetadata, EdgeError, NetworkEgress, NetworkIngress,
    NetworkPrincipal, NetworkTarget, PathMetadata, ProtocolToken, SessionControl, TransportSession,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::{
    BridgeCapability, BridgeIdentity, BridgeRefusal, DATA_FRAME_CAPACITY, SESSION_CAPACITY,
};

/// 2 carries the `NetworkPrincipal`/`NetworkTarget` union in `Open` and
/// `OpenRequest` (HOST-CONTROL §3). A version-1 registration is refused with
/// [`RegistrationReply::Refused`] before any frame is read, and a version-1
/// frame header stops the reader the way a corrupt frame already did.
pub const WIRE_VERSION: u8 = 2;
pub const DATA_CHUNK: usize = 64 * 1024;
const MAGIC: [u8; 4] = *b"KBRG";
const HEADER_LEN: usize = 18;
const MAX_CONTROL: usize = 64 * 1024;
const WRITE_CAPACITY: usize = SESSION_CAPACITY * DATA_FRAME_CAPACITY;
const CONTROL_CAPACITY: usize = 4 * SESSION_CAPACITY;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    pub identity: BridgeIdentity,
    pub capability: BridgeCapability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistrationReply {
    Accepted,
    Refused(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Agent,
    Gateway,
}

impl Side {
    fn first_session(self) -> u64 {
        match self {
            Self::Agent => 2,
            Self::Gateway => 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenMetadata {
    principal: NetworkPrincipal,
    protocol: ProtocolToken,
    path: PathMetadata,
    connection_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    target: NetworkTarget,
    protocol: ProtocolToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Open = 1,
    OpenRequest = 2,
    Data = 3,
    HalfClose = 4,
    Reset = 5,
    Cancel = 6,
    Connected = 7,
    Disconnected = 8,
}

impl Kind {
    fn parse(value: u8) -> std::io::Result<Self> {
        match value {
            1 => Ok(Self::Open),
            2 => Ok(Self::OpenRequest),
            3 => Ok(Self::Data),
            4 => Ok(Self::HalfClose),
            5 => Ok(Self::Reset),
            6 => Ok(Self::Cancel),
            7 => Ok(Self::Connected),
            8 => Ok(Self::Disconnected),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown bridge frame kind",
            )),
        }
    }
}

struct WireFrame {
    kind: Kind,
    session_id: u64,
    body: Vec<u8>,
}

struct Route {
    inbound: Option<koru_network_edge::ByteSender>,
    outbound_open: bool,
    control: SessionControl,
}

struct Core {
    writer: mpsc::Sender<WireFrame>,
    control: mpsc::Sender<WireFrame>,
    routes: Arc<Mutex<HashMap<u64, Route>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<TransportSession, EdgeError>>>>>,
    next_session: AtomicU64,
    closed: AtomicBool,
    runtime: tokio::runtime::Handle,
}

/// One authenticated long-lived local bridge connection.
#[derive(Clone)]
pub struct Link {
    core: Arc<Core>,
    accepted: Arc<tokio::sync::Mutex<mpsc::Receiver<TransportSession>>>,
    connections: Arc<tokio::sync::Mutex<mpsc::Receiver<ConnectionEvent>>>,
}

impl Link {
    pub fn attach(
        stream: koru_ipc::Stream,
        side: Side,
        egress: Option<Arc<dyn NetworkEgress>>,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "the bridge must attach inside a Tokio runtime".to_owned())?;
        let (mut reader, mut output) = koru_ipc::split(stream);
        let (writer, mut outgoing) = mpsc::channel::<WireFrame>(WRITE_CAPACITY);
        let (control, mut controls) = mpsc::channel::<WireFrame>(CONTROL_CAPACITY);
        let (accepted_tx, accepted_rx) = mpsc::channel(SESSION_CAPACITY);
        let (connection_tx, connection_rx) = mpsc::channel(SESSION_CAPACITY);
        let routes = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let core = Arc::new(Core {
            writer,
            control,
            routes: Arc::clone(&routes),
            pending: Arc::clone(&pending),
            next_session: AtomicU64::new(side.first_session()),
            closed: AtomicBool::new(false),
            runtime: runtime.clone(),
        });

        let writer_core = Arc::clone(&core);
        std::thread::Builder::new()
            .name("koru network bridge writer".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut controls_open = true;
                    let mut outgoing_open = true;
                    loop {
                        let frame = tokio::select! {
                            biased;
                            frame = controls.recv(), if controls_open => match frame {
                                Some(frame) => Some(frame),
                                None => {
                                    controls_open = false;
                                    continue;
                                }
                            },
                            frame = outgoing.recv(), if outgoing_open => match frame {
                                Some(frame) => Some(frame),
                                None => {
                                    outgoing_open = false;
                                    continue;
                                }
                            },
                            else => None,
                        };
                        let Some(frame) = frame else {
                            break;
                        };
                        if let Err(error) = write_wire(&mut output, frame) {
                            tracing::warn!(%error, "network bridge writer stopped");
                            break;
                        }
                    }
                    writer_core.closed.store(true, Ordering::Release);
                });
            })
            .map_err(|error| error.to_string())?;

        let reader_core = Arc::clone(&core);
        std::thread::Builder::new()
            .name("koru network bridge reader".into())
            .spawn(move || {
                let _shutdown = ReaderShutdown::new(Arc::clone(&reader_core));
                loop {
                    match read_wire(&mut reader) {
                        Ok(frame) => {
                            if let Err(reason) = dispatch(
                                &reader_core,
                                &accepted_tx,
                                &connection_tx,
                                egress.as_ref(),
                                frame,
                            ) {
                                tracing::warn!(reason, "network bridge reader stopped");
                                break;
                            }
                        }
                        Err(error) => {
                            if error.kind() != std::io::ErrorKind::UnexpectedEof {
                                tracing::warn!(%error, "network bridge reader stopped");
                            }
                            break;
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        Ok(Self {
            core,
            accepted: Arc::new(tokio::sync::Mutex::new(accepted_rx)),
            connections: Arc::new(tokio::sync::Mutex::new(connection_rx)),
        })
    }

    /// Forward one locally-owned session. Admission is synchronous and bounded;
    /// payload pumping runs on the caller's Tokio runtime.
    pub fn admit(&self, session: TransportSession) -> Result<(), BridgeRefusal> {
        let session_id = self.next_id();
        admit_with_id(&self.core, session_id, session).map_err(|error| match error {
            EdgeError::Closed => BridgeRefusal::Saturated,
            _ => BridgeRefusal::Saturated,
        })
    }

    pub async fn accepted(&self) -> Option<TransportSession> {
        self.accepted.lock().await.recv().await
    }

    pub async fn connection_event(&self) -> Option<ConnectionEvent> {
        self.connections.lock().await.recv().await
    }

    pub fn is_closed(&self) -> bool {
        self.core.closed.load(Ordering::Acquire)
            || self.core.writer.is_closed()
            || self.core.control.is_closed()
    }

    pub async fn open(
        &self,
        target: NetworkTarget,
        protocol: ProtocolToken,
    ) -> Result<TransportSession, EdgeError> {
        let session_id = self.next_id();
        let body =
            serde_json::to_vec(&OpenRequest { target, protocol }).map_err(|_| EdgeError::Closed)?;
        let (answer, waiting) = oneshot::channel();
        self.core
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id, answer);
        if self
            .core
            .control
            .send(WireFrame {
                kind: Kind::OpenRequest,
                session_id,
                body,
            })
            .await
            .is_err()
        {
            self.core
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&session_id);
            return Err(EdgeError::Closed);
        }
        waiting.await.unwrap_or(Err(EdgeError::Closed))
    }

    fn next_id(&self) -> u64 {
        self.core.next_session.fetch_add(2, Ordering::Relaxed)
    }
}

impl NetworkEgress for Link {
    fn open(
        &self,
        target: NetworkTarget,
        protocol: ProtocolToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TransportSession, EdgeError>> + Send + '_>,
    > {
        Box::pin(self.open(target, protocol))
    }
}

impl NetworkIngress for Link {
    fn admit(&self, session: TransportSession) -> Result<(), EdgeError> {
        Link::admit(self, session).map_err(|_| EdgeError::Closed)
    }

    fn connected(&self, connection: ConnectionMetadata) -> Result<(), EdgeError> {
        let body = serde_json::to_vec(&connection).map_err(|_| EdgeError::Closed)?;
        self.core
            .control
            .try_send(WireFrame {
                kind: Kind::Connected,
                session_id: connection.connection_id,
                body,
            })
            .map_err(|_| EdgeError::Closed)
    }

    fn disconnected(&self, connection_id: u64) {
        if self
            .core
            .control
            .try_send(WireFrame {
                kind: Kind::Disconnected,
                session_id: connection_id,
                body: Vec::new(),
            })
            .is_err()
        {
            tracing::warn!(
                connection_id,
                "bridge control lane refused a disconnect event"
            );
        }
    }
}

fn admit_with_id(
    core: &Arc<Core>,
    session_id: u64,
    mut session: TransportSession,
) -> Result<(), EdgeError> {
    let metadata = OpenMetadata {
        principal: session.principal.clone(),
        protocol: session.protocol.clone(),
        path: session.path.clone(),
        connection_id: session.connection_id,
    };
    let body = serde_json::to_vec(&metadata).map_err(|_| EdgeError::Closed)?;
    let control = session.control();
    core.routes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            session_id,
            Route {
                inbound: Some(session.send.clone()),
                outbound_open: true,
                control,
            },
        );
    core.control
        .try_send(WireFrame {
            kind: Kind::Open,
            session_id,
            body,
        })
        .map_err(|_| {
            core.routes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&session_id);
            EdgeError::Closed
        })?;
    session.session_id = session_id;
    spawn_outbound_pump(Arc::clone(core), session_id, session.receive);
    Ok(())
}

fn spawn_outbound_pump(
    core: Arc<Core>,
    session_id: u64,
    mut receive: koru_network_edge::ByteReceiver,
) {
    core.runtime.clone().spawn(async move {
        loop {
            match receive.recv().await {
                Ok(Some(bytes)) => {
                    for chunk in bytes.chunks(DATA_CHUNK) {
                        if core
                            .writer
                            .send(WireFrame {
                                kind: Kind::Data,
                                session_id,
                                body: chunk.to_vec(),
                            })
                            .await
                            .is_err()
                        {
                            reset_route(&core, session_id, 0x4252_4944);
                            return;
                        }
                    }
                }
                Ok(None) => {
                    let _ = core
                        .writer
                        .send(WireFrame {
                            kind: Kind::HalfClose,
                            session_id,
                            body: Vec::new(),
                        })
                        .await;
                    finish_outbound(&core, session_id);
                    return;
                }
                Err(EdgeError::Cancelled) => {
                    let _ = core
                        .writer
                        .send(WireFrame {
                            kind: Kind::Cancel,
                            session_id,
                            body: Vec::new(),
                        })
                        .await;
                    remove_route(&core, session_id);
                    return;
                }
                Err(EdgeError::Reset(code)) => {
                    let _ = core
                        .writer
                        .send(WireFrame {
                            kind: Kind::Reset,
                            session_id,
                            body: code.to_le_bytes().to_vec(),
                        })
                        .await;
                    remove_route(&core, session_id);
                    return;
                }
                Err(EdgeError::Closed | EdgeError::InvalidProtocol) => {
                    finish_outbound(&core, session_id);
                    return;
                }
            }
        }
    });
}

fn dispatch(
    core: &Arc<Core>,
    accepted: &mpsc::Sender<TransportSession>,
    connections: &mpsc::Sender<ConnectionEvent>,
    egress: Option<&Arc<dyn NetworkEgress>>,
    frame: WireFrame,
) -> Result<(), &'static str> {
    match frame.kind {
        Kind::Open => {
            let metadata: OpenMetadata =
                serde_json::from_slice(&frame.body).map_err(|_| "invalid open metadata")?;
            let (mut local, bridge) = TransportSession::pair(
                metadata.principal.clone(),
                metadata.principal,
                metadata.protocol,
                DATA_FRAME_CAPACITY,
            );
            local.session_id = frame.session_id;
            local.connection_id = metadata.connection_id;
            local.path = metadata.path;
            let control = bridge.control();
            core.routes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    frame.session_id,
                    Route {
                        inbound: Some(bridge.send.clone()),
                        outbound_open: true,
                        control,
                    },
                );
            spawn_outbound_pump(Arc::clone(core), frame.session_id, bridge.receive);
            if let Some(waiting) = core
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&frame.session_id)
            {
                let _ = waiting.send(Ok(local));
            } else {
                accepted
                    .blocking_send(local)
                    .map_err(|_| "session receiver closed")?;
            }
        }
        Kind::OpenRequest => {
            let request: OpenRequest =
                serde_json::from_slice(&frame.body).map_err(|_| "invalid open request")?;
            let Some(egress) = egress.cloned() else {
                send_reset(core, frame.session_id, 0x4e4f_4547);
                return Ok(());
            };
            let core = Arc::clone(core);
            core.runtime.clone().spawn(async move {
                match egress.open(request.target, request.protocol).await {
                    Ok(session) => {
                        if admit_with_id(&core, frame.session_id, session).is_err() {
                            send_reset(&core, frame.session_id, 0x4252_4944);
                        }
                    }
                    Err(_) => send_reset(&core, frame.session_id, 0x4f50_454e),
                }
            });
        }
        Kind::Data => {
            let sender = core
                .routes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&frame.session_id)
                .and_then(|route| route.inbound.clone());
            let Some(sender) = sender else {
                // A reset, cancellation, or local receiver drop may overtake a
                // payload already in the OS pipe. That closes this logical
                // stream, never the process-wide bridge used by every peer.
                send_reset(core, frame.session_id, 0x5354_414c);
                return Ok(());
            };
            if sender.blocking_send(frame.body).is_err() {
                reset_route(core, frame.session_id, 0x5243_5643);
                send_reset(core, frame.session_id, 0x5243_5643);
            }
        }
        Kind::HalfClose => {
            let mut routes = core
                .routes
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let remove = if let Some(route) = routes.get_mut(&frame.session_id) {
                route.inbound = None;
                !route.outbound_open
            } else {
                false
            };
            if remove {
                routes.remove(&frame.session_id);
            }
        }
        Kind::Reset => {
            let code = frame
                .body
                .get(..8)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_le_bytes)
                .unwrap_or_default();
            reset_route(core, frame.session_id, code);
            if let Some(waiting) = core
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&frame.session_id)
            {
                let _ = waiting.send(Err(EdgeError::Reset(code)));
            }
        }
        Kind::Cancel => {
            if let Some(route) = core
                .routes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&frame.session_id)
            {
                route.control.cancel();
            }
        }
        Kind::Connected => {
            let metadata: ConnectionMetadata =
                serde_json::from_slice(&frame.body).map_err(|_| "invalid connection metadata")?;
            connections
                .blocking_send(ConnectionEvent::Connected(metadata))
                .map_err(|_| "connection event receiver closed")?;
        }
        Kind::Disconnected => {
            connections
                .blocking_send(ConnectionEvent::Disconnected {
                    connection_id: frame.session_id,
                })
                .map_err(|_| "connection event receiver closed")?;
        }
    }
    Ok(())
}

fn send_reset(core: &Arc<Core>, session_id: u64, code: u64) {
    let _ = core.control.try_send(WireFrame {
        kind: Kind::Reset,
        session_id,
        body: code.to_le_bytes().to_vec(),
    });
}

fn reset_route(core: &Arc<Core>, session_id: u64, code: u64) {
    if let Some(route) = core
        .routes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&session_id)
    {
        route.control.reset(code);
    }
}

fn remove_route(core: &Arc<Core>, session_id: u64) {
    core.routes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&session_id);
}

fn finish_outbound(core: &Arc<Core>, session_id: u64) {
    let mut routes = core
        .routes
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let remove = if let Some(route) = routes.get_mut(&session_id) {
        route.outbound_open = false;
        route.inbound.is_none()
    } else {
        false
    };
    if remove {
        routes.remove(&session_id);
    }
}

fn fail_all(core: &Arc<Core>, code: u64) {
    let routes = std::mem::take(
        &mut *core
            .routes
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    );
    for (_, route) in routes {
        route.control.reset(code);
    }
    let pending = std::mem::take(
        &mut *core
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    );
    for (_, waiting) in pending {
        let _ = waiting.send(Err(EdgeError::Reset(code)));
    }
}

struct ReaderShutdown {
    core: Arc<Core>,
}

impl ReaderShutdown {
    fn new(core: Arc<Core>) -> Self {
        Self { core }
    }
}

impl Drop for ReaderShutdown {
    fn drop(&mut self) {
        self.core.closed.store(true, Ordering::Release);
        fail_all(&self.core, 0x4252_4944);
    }
}

fn write_wire(writer: &mut impl Write, frame: WireFrame) -> std::io::Result<()> {
    let cap = match frame.kind {
        Kind::Data => DATA_CHUNK,
        _ => MAX_CONTROL,
    };
    if frame.body.len() > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bridge frame exceeds its bound",
        ));
    }
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = WIRE_VERSION;
    header[5] = frame.kind as u8;
    header[6..14].copy_from_slice(&frame.session_id.to_le_bytes());
    header[14..18].copy_from_slice(&(frame.body.len() as u32).to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(&frame.body)?;
    writer.flush()
}

fn read_wire(reader: &mut impl Read) -> std::io::Result<WireFrame> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC || header[4] != WIRE_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bridge framing/version mismatch",
        ));
    }
    let kind = Kind::parse(header[5])?;
    let session_id = u64::from_le_bytes(header[6..14].try_into().expect("fixed session id"));
    let body_len =
        u32::from_le_bytes(header[14..18].try_into().expect("fixed body length")) as usize;
    let cap = if kind == Kind::Data {
        DATA_CHUNK
    } else {
        MAX_CONTROL
    };
    if body_len > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bridge frame declared an oversized body",
        ));
    }
    let mut body = vec![0; body_len];
    reader.read_exact(&mut body)?;
    Ok(WireFrame {
        kind,
        session_id,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_device::cloud::{CloudId, CloudPrincipalV1, CloudReplicaPrincipalV1};
    use koru_device::{DeviceId, RelationshipId, ReplicationEndpointSecret};
    use koru_network_edge::{DevicePrincipal, PathKind, ProtocolToken};

    fn principal() -> NetworkPrincipal {
        DevicePrincipal {
            device_id: DeviceId::new(),
            relationship_id: RelationshipId::new(),
            endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
            authorization_revision: 4,
        }
        .into()
    }

    fn cloud_id(byte: u8) -> CloudId {
        let mut bytes = [byte; 16];
        bytes[6] = 0x40 | (bytes[6] & 0x0f);
        bytes[8] = 0x80 | (bytes[8] & 0x3f);
        CloudId::from_bytes(bytes)
    }

    fn dataset() -> CloudReplicaPrincipalV1 {
        CloudReplicaPrincipalV1::Workspace {
            workspace_id: cloud_id(0x44),
            incarnation_id: cloud_id(0x55),
            logical_replica_id: cloud_id(0x66),
        }
    }

    fn cloud_principal() -> NetworkPrincipal {
        CloudPrincipalV1 {
            version: 1,
            permit_id: cloud_id(0x11).as_uuid(),
            account_id: cloud_id(0x22),
            profile_id: cloud_id(0x33),
            dataset: dataset(),
            client_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
            worker_endpoint_id: ReplicationEndpointSecret::generate().endpoint_id(),
            credential_id: cloud_id(0x77),
            authorization_version: 3,
            membership_epoch: 4,
            cloud_access_epoch: 5,
            authority_workspace_id: cloud_id(0x88),
            authority_workspace_incarnation_id: cloud_id(0x99),
            control_session_id: cloud_id(0xaa).as_uuid(),
            account_generation: 2,
            session_generation: 1,
            expires_at_seconds: 1_800_000_000,
            credential_issued_at_seconds: None,
        }
        .into()
    }

    async fn links() -> (Link, Link) {
        links_with_egress(None).await
    }

    async fn links_with_egress(egress: Option<Arc<dyn NetworkEgress>>) -> (Link, Link) {
        let root = tempfile::tempdir().unwrap();
        let endpoint = koru_ipc::Endpoint::for_service_root(
            koru_ipc::AGENT_GATEWAY_BRIDGE,
            "internal-debug",
            root.path(),
        )
        .unwrap();
        let listener = koru_ipc::Listener::bind(endpoint.clone()).unwrap();
        let accepting = tokio::task::spawn_blocking(move || listener.accept().unwrap());
        let gateway_stream = koru_ipc::connect(&endpoint).unwrap();
        let agent_stream = accepting.await.unwrap();
        (
            Link::attach(agent_stream, Side::Agent, egress).unwrap(),
            Link::attach(gateway_stream, Side::Gateway, None).unwrap(),
        )
    }

    /// Answers every open by handing back one half of a local pair, and keeps
    /// the target it was asked for so a test can prove the union survived the
    /// crossing rather than being flattened into a device id.
    #[derive(Clone, Default)]
    struct RecordingEgress {
        targets: Arc<Mutex<Vec<NetworkTarget>>>,
        /// The far halves of the pairs this egress answered with. Dropping one
        /// would half-close its stream immediately, so the fake keeps them for
        /// as long as the test does.
        held: Arc<Mutex<Vec<TransportSession>>>,
    }

    impl NetworkEgress for RecordingEgress {
        fn open(
            &self,
            target: NetworkTarget,
            protocol: ProtocolToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<TransportSession, EdgeError>> + Send + '_>,
        > {
            let principal = match target {
                NetworkTarget::Device { .. } => principal(),
                NetworkTarget::Cloud { .. } => cloud_principal(),
            };
            let (held, opened) =
                TransportSession::pair(principal.clone(), principal, protocol, DATA_FRAME_CAPACITY);
            self.targets
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(target);
            self.held
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(held);
            Box::pin(async move { Ok(opened) })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lifecycle_frames_survive_a_saturated_data_lane() {
        let (writer, _outgoing) = mpsc::channel(WRITE_CAPACITY);
        let (control, mut controls) = mpsc::channel(CONTROL_CAPACITY);
        for session_id in 0..WRITE_CAPACITY as u64 {
            writer
                .try_send(WireFrame {
                    kind: Kind::Data,
                    session_id,
                    body: vec![0],
                })
                .unwrap();
        }
        assert!(writer.try_reserve().is_err());
        let (_accepted_tx, accepted_rx) = mpsc::channel(SESSION_CAPACITY);
        let (_connection_tx, connection_rx) = mpsc::channel(SESSION_CAPACITY);
        let link = Link {
            core: Arc::new(Core {
                writer,
                control,
                routes: Arc::new(Mutex::new(HashMap::new())),
                pending: Arc::new(Mutex::new(HashMap::new())),
                next_session: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                runtime: tokio::runtime::Handle::current(),
            }),
            accepted: Arc::new(tokio::sync::Mutex::new(accepted_rx)),
            connections: Arc::new(tokio::sync::Mutex::new(connection_rx)),
        };
        let connection = ConnectionMetadata {
            connection_id: 44,
            principal: principal(),
            path: PathMetadata {
                kind: PathKind::Direct,
                outbound: true,
                selected_is_ip: false,
            },
        };

        NetworkIngress::connected(&link, connection).unwrap();
        NetworkIngress::disconnected(&link, 44);

        assert_eq!(controls.try_recv().unwrap().kind, Kind::Connected);
        assert_eq!(controls.try_recv().unwrap().kind, Kind::Disconnected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poisoned_route_state_does_not_disable_the_bridge() {
        let (agent, gateway) = links().await;
        let routes = Arc::clone(&agent.core.routes);
        let _ = std::panic::catch_unwind(move || {
            let _guard = routes.lock().unwrap();
            panic!("poison bridge routes for the regression test");
        });
        let (_peer, admitted) = TransportSession::pair(
            principal(),
            principal(),
            ProtocolToken::parse("koru/replication/1").unwrap(),
            DATA_FRAME_CAPACITY,
        );

        agent.admit(admitted).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), gateway.accepted())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opaque_bytes_cross_both_directions_and_half_close() {
        let (agent, gateway) = links().await;
        let protocol = ProtocolToken::parse("koru/replication/1").unwrap();
        let (mut peer, mut admitted) =
            TransportSession::pair(principal(), principal(), protocol, DATA_FRAME_CAPACITY);
        admitted.connection_id = 91;
        admitted.path = PathMetadata {
            kind: PathKind::Direct,
            outbound: false,
            selected_is_ip: false,
        };
        agent.admit(admitted).unwrap();
        let mut received = gateway.accepted().await.unwrap();
        assert_eq!(received.connection_id, 91);
        assert_eq!(received.path.kind, PathKind::Direct);

        peer.send.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(received.receive.recv().await.unwrap(), Some(vec![1, 2, 3]));
        received.send.send(vec![4, 5]).await.unwrap();
        assert_eq!(peer.receive.recv().await.unwrap(), Some(vec![4, 5]));

        drop(peer.send);
        assert_eq!(received.receive.recv().await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reset_is_terminal_and_payload_is_never_replayed() {
        let (agent, gateway) = links().await;
        let (peer, admitted) = TransportSession::pair(
            principal(),
            principal(),
            ProtocolToken::parse("koru/replication/1").unwrap(),
            DATA_FRAME_CAPACITY,
        );
        agent.admit(admitted).unwrap();
        let mut received = gateway.accepted().await.unwrap();
        peer.reset(77);
        drop(peer);
        assert_eq!(received.receive.recv().await, Err(EdgeError::Reset(77)));
        assert!(matches!(
            received.receive.recv().await,
            Ok(None) | Err(EdgeError::Reset(77))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_closed_logical_receiver_does_not_kill_the_process_bridge() {
        let (agent, gateway) = links().await;
        let protocol = ProtocolToken::parse("koru/replication/1").unwrap();
        let (first_peer, first_admitted) = TransportSession::pair(
            principal(),
            principal(),
            protocol.clone(),
            DATA_FRAME_CAPACITY,
        );
        agent.admit(first_admitted).unwrap();
        let first_received = gateway.accepted().await.unwrap();

        // The protocol task completed and dropped its receiver while a payload
        // was already on the way. This is a stream teardown race, not a reason
        // to terminate the one bridge carrying every later peer and session.
        drop(first_received);
        first_peer.send.send(vec![1, 2, 3]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;

        let connection = ConnectionMetadata {
            connection_id: 92,
            principal: principal(),
            path: PathMetadata {
                kind: PathKind::Relay,
                outbound: true,
                selected_is_ip: false,
            },
        };
        NetworkIngress::connected(&agent, connection.clone()).unwrap();
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gateway.connection_event(),
        )
        .await
        .expect("the bridge stopped forwarding connection events")
        .expect("the bridge connection-event receiver closed");
        assert_eq!(event, ConnectionEvent::Connected(connection));

        let (mut second_peer, second_admitted) =
            TransportSession::pair(principal(), principal(), protocol, DATA_FRAME_CAPACITY);
        agent.admit(second_admitted).unwrap();
        let second_received =
            tokio::time::timeout(std::time::Duration::from_secs(1), gateway.accepted())
                .await
                .expect("the bridge stopped accepting logical sessions")
                .expect("the bridge reader exited");
        second_received.send.send(vec![4, 5, 6]).await.unwrap();
        assert_eq!(
            second_peer.receive.recv().await.unwrap(),
            Some(vec![4, 5, 6])
        );
        assert!(!agent.is_closed());
        assert!(!gateway.is_closed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fully_half_closed_sessions_release_their_routes() {
        let (agent, gateway) = links().await;
        let (peer, admitted) = TransportSession::pair(
            principal(),
            principal(),
            ProtocolToken::parse("koru/replication/1").unwrap(),
            DATA_FRAME_CAPACITY,
        );
        agent.admit(admitted).unwrap();
        let received = gateway.accepted().await.unwrap();
        drop(peer);
        drop(received);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if agent.core.routes.lock().unwrap().is_empty()
                    && gateway.core.routes.lock().unwrap().is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fully closed logical sessions leaked bridge routes");
    }

    /// **Both principal arms cross unchanged.** The bridge carries the union
    /// rather than a device id, so a Cloud session admitted on one side arrives
    /// as a Cloud session on the other — the gateway must be able to refuse it
    /// from the peer path, which it cannot do if the arm was lost in transit.
    #[tokio::test(flavor = "multi_thread")]
    async fn both_principal_arms_survive_the_bridge() {
        let (agent, gateway) = links().await;

        let (_device_peer, device_admitted) = TransportSession::pair(
            principal(),
            principal(),
            ProtocolToken::parse("koru/replication/1").unwrap(),
            DATA_FRAME_CAPACITY,
        );
        agent.admit(device_admitted).unwrap();
        let received = gateway.accepted().await.unwrap();
        assert!(received.principal.device().is_some());

        let cloud = cloud_principal();
        let (_cloud_peer, cloud_admitted) = TransportSession::pair(
            cloud.clone(),
            cloud.clone(),
            ProtocolToken::parse("koru-cloud-content-v1").unwrap(),
            DATA_FRAME_CAPACITY,
        );
        agent.admit(cloud_admitted).unwrap();
        let received = gateway.accepted().await.unwrap();
        assert_eq!(received.principal, cloud);
        assert_eq!(received.protocol.as_str(), "koru-cloud-content-v1");
    }

    /// **Both target arms cross unchanged**, and a Cloud target carries no
    /// socket, issuer or credential — only the permit id and the dataset the
    /// Agent will resolve against its own live permit.
    #[tokio::test(flavor = "multi_thread")]
    async fn both_target_arms_survive_the_bridge() {
        let egress = Arc::new(RecordingEgress::default());
        let (_agent, gateway) = links_with_egress(Some(egress.clone())).await;

        let device_id = DeviceId::new();
        let opened = gateway
            .open(
                NetworkTarget::Device { device_id },
                ProtocolToken::parse("koru/replication/1").unwrap(),
            )
            .await
            .expect("the device target opens");
        assert!(opened.principal.device().is_some());

        let permit_id = cloud_id(0x11).as_uuid();
        let opened = gateway
            .open(
                NetworkTarget::Cloud {
                    permit_id,
                    dataset: dataset(),
                },
                ProtocolToken::parse("koru-cloud-content-v1").unwrap(),
            )
            .await
            .expect("the Cloud target opens");
        assert!(opened.principal.cloud().is_some());

        let targets = egress
            .targets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(
            targets,
            vec![
                NetworkTarget::Device { device_id },
                NetworkTarget::Cloud {
                    permit_id,
                    dataset: dataset()
                },
            ]
        );
    }

    /// A frame stamped with the previous wire version stops the reader, the
    /// same way a corrupt header already did. Version 2 is not negotiated
    /// frame by frame; it is refused at registration and, if a frame still
    /// arrives, here.
    #[test]
    fn a_version_one_frame_header_is_refused() {
        let mut encoded = Vec::new();
        write_wire(
            &mut encoded,
            WireFrame {
                kind: Kind::Data,
                session_id: 3,
                body: vec![9, 9, 9],
            },
        )
        .unwrap();
        assert_eq!(encoded[4], WIRE_VERSION);
        assert!(read_wire(&mut encoded.as_slice()).is_ok());

        let mut previous = encoded.clone();
        previous[4] = WIRE_VERSION - 1;
        let error = read_wire(&mut previous.as_slice())
            .err()
            .expect("a version-1 frame is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    /// Unknown fields and unknown variants close admission rather than
    /// selecting the other arm of the union (HOST-CONTROL §3).
    #[test]
    fn unknown_open_fields_and_variants_are_refused() {
        let metadata = OpenMetadata {
            principal: cloud_principal(),
            protocol: ProtocolToken::parse("koru-cloud-content-v1").unwrap(),
            path: PathMetadata {
                kind: PathKind::Direct,
                outbound: false,
                selected_is_ip: false,
            },
            connection_id: 7,
        };
        let mut encoded: serde_json::Value = serde_json::to_value(&metadata).unwrap();
        assert!(serde_json::from_value::<OpenMetadata>(encoded.clone()).is_ok());
        encoded["lease_id"] = serde_json::json!("00000000-0000-4000-8000-000000000000");
        assert!(serde_json::from_value::<OpenMetadata>(encoded).is_err());

        let request = OpenRequest {
            target: NetworkTarget::Cloud {
                permit_id: cloud_id(0x11).as_uuid(),
                dataset: dataset(),
            },
            protocol: ProtocolToken::parse("koru-cloud-content-v1").unwrap(),
        };
        let mut encoded: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert!(serde_json::from_value::<OpenRequest>(encoded.clone()).is_ok());
        encoded["target"]["kind"] = serde_json::json!("worker");
        assert!(serde_json::from_value::<OpenRequest>(encoded).is_err());
    }
}
