// SPDX-License-Identifier: MPL-2.0

//! The owner server (M8, P8.3 piece H2): the engine that serves a linked
//! device's Bot streams on the computer that runs the Bots.
//!
//! The carrier authenticated the device and wrote no frame; the gateway's
//! adapter read the stream's hello and asked the admission port. A stream
//! arrives here with its hello unanswered ([`BotOwnerServer::serve`]). This
//! module answers it and serves the stream in its role:
//!
//! - **feed** ([`feed`]): the owner's committed changes, read by cursor;
//! - **command** ([`command`]): one intent at a time, each with a durable
//!   receipt written before it is applied, and reads that change nothing;
//! - **live** ([`live`]): the events of a running reply, never saved.
//!
//! **What a linked device can cause here is the allow-list in [`command`],
//! and nothing else.** Every command it sends is one of eleven typed kinds;
//! a kind outside them is answered `executing_computer_only`. The engine
//! runs each as the existing local command, with that command's own checks
//! unchanged, and it is the only place that lends a linked device the scope
//! those local commands ask for. A linked device never names a scope, a
//! person or a local command.
//!
//! **Every message is checked** (contract 4.2): the stream's hello
//! succeeded, the device is still one the admission port names a client with
//! the scope the message needs, the message names this computer, it belongs
//! to the stream's role and to a negotiated capability, and what it names is
//! a Bot of this person on this computer that is copied to linked desktops.
//!
//! **Nothing here blocks the AI writer or the caller of `serve`.** A stream
//! is a task on the runtime that admitted it. It reaches `ai.db` through
//! short calls on the writer ([`writer`]), asks the Store on blocking
//! threads, and runs a local command through the worker's own port, the way
//! a window's request does.
//!
//! Nothing a person or a Bot wrote is logged from here: a refusal is logged
//! by its reason, a command by its kind and outcome.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_ai_api::AiServerPort;
use koru_ai_core::Timestamp;
use koru_ai_core::bots::{BotAuthority, BotOwner, DeviceId};
use koru_bot_client_wire::{
    BUILT_CAPABILITIES, ClientHello, FrameDecoder, Negotiated, Refused, Role, WireMessage, encode,
    limits, refusal,
};
use koru_network_edge::TransportSession;

use super::feed::{FeedRefusal, HomeCopies, OwnerFeed, PageOutcome, Reconciled};
use crate::worker::{Envelope, WorkerState};

pub(crate) mod command;
mod feed;
mod live;
pub(in crate::worker) mod writer;

pub(in crate::worker) use writer::{Writer, WriterCall, WriterHint};

/// Whether a device is still a client of this computer's Bots, and with
/// which scopes: the admission port, read again for every message that asks
/// for something. `None` when the port does not name the device a client.
pub(crate) trait ClientAdmission: Send + Sync + 'static {
    fn client_scopes(&self, device: &DeviceId) -> Option<BTreeSet<String>>;
}

impl ClientAdmission for koru_store_server::BotPeers {
    fn client_scopes(&self, device: &DeviceId) -> Option<BTreeSet<String>> {
        self.current().client_scopes(device).cloned()
    }
}

/// One Bot stream the admission port let in, with its hello unanswered.
pub struct BotClientStream {
    /// The stream's first frame.
    pub hello: ClientHello,
    /// The device the carrier authenticated.
    pub device: DeviceId,
    /// The admission port's scopes for that device when the hello was read.
    pub scopes: BTreeSet<String>,
    pub session: TransportSession,
    /// The decoder that read the hello; whatever the client sent after it is
    /// still inside.
    pub decoder: FrameDecoder,
}

impl std::fmt::Debug for BotClientStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BotClientStream")
            .field("role", &self.hello.role)
            .field("scopes", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

/// How long the server waits, each from contract 2.4 and 2.5.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// An idle command stream is closed after this long.
    pub(crate) command_idle: Duration,
    /// A page left unacknowledged this long disconnects the client.
    pub(crate) ack: Duration,
    /// The keepalive interval on an idle feed.
    pub(crate) keepalive: Duration,
    /// How often the markers are reconciled with the Store while a linked
    /// desktop is subscribed.
    pub(crate) reconcile: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            command_idle: Duration::from_millis(limits::COMMAND_IDLE_TIMEOUT_MS),
            ack: Duration::from_millis(limits::ACK_TIMEOUT_MS),
            keepalive: Duration::from_millis(limits::KEEPALIVE_INTERVAL_MS),
            reconcile: super::feed::RECONCILE_EVERY,
        }
    }
}

/// What the worker hands over for an owner server to be built from: its
/// inbox, its Bot authority, the admission port, and the feed's head as the
/// writer publishes it.
#[derive(Clone)]
pub(crate) struct Parts {
    pub(in crate::worker) sender: SyncSender<Envelope>,
    pub(in crate::worker) shutdown: Arc<AtomicBool>,
    pub(crate) authority: Arc<dyn BotAuthority>,
    pub(crate) admission: Arc<dyn ClientAdmission>,
    pub(in crate::worker) head: tokio::sync::watch::Receiver<u64>,
    pub(in crate::worker) feed_streams: Arc<AtomicUsize>,
}

/// The owner server. Cheap to clone; every clone is the same server.
#[derive(Clone)]
pub struct BotOwnerServer {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BotOwnerServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BotOwnerServer")
            .finish_non_exhaustive()
    }
}

impl BotOwnerServer {
    /// `port` runs the local commands: in production the worker's own
    /// client, the one a window's request goes through.
    pub(crate) fn new(parts: Parts, port: Arc<dyn AiServerPort>, timing: Timing) -> Self {
        Self::with_local(parts, Arc::new(command::PortCommands(port)), timing)
    }

    /// [`Self::new`] with the local commands given as they are run.
    pub(crate) fn with_local(
        parts: Parts,
        local: Arc<dyn command::LocalCommands>,
        timing: Timing,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                link: Link {
                    sender: parts.sender,
                    shutdown: parts.shutdown,
                },
                local,
                authority: parts.authority,
                admission: parts.admission,
                peers: Mutex::new(Peers::default()),
                starting: tokio::sync::Mutex::new(()),
                reconciling: tokio::sync::Mutex::new(None),
                head: parts.head,
                feed_streams: parts.feed_streams,
                timing,
            }),
        }
    }

    /// Take over one admitted stream: answer its hello and serve it in its
    /// role. **Returns at once**; the stream is served on its own task of
    /// the runtime this is called in. Called outside a runtime, the stream
    /// is reset and nothing is served.
    pub fn serve(&self, stream: BotClientStream) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            stream.session.reset(1);
            return;
        };
        let shared = Arc::clone(&self.shared);
        runtime.spawn(async move { shared.run(stream).await });
    }
}

/// The worker is stopping, or its inbox stayed full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gone;

/// The way to the AI writer. A call is one short piece of work on the
/// writer thread; the caller waits for its answer without holding a thread.
#[derive(Clone)]
struct Link {
    sender: SyncSender<Envelope>,
    shutdown: Arc<AtomicBool>,
}

impl Link {
    /// Whether the worker is stopping.
    fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// How long a call waits for room in the writer's inbox before it gives
    /// up, in steps of [`Self::FULL_STEP`].
    const FULL_TRIES: u32 = 300;
    const FULL_STEP: Duration = Duration::from_millis(10);

    async fn on_writer<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut WorkerState) -> T + Send + 'static,
    ) -> Result<T, Gone> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let call: WriterCall = Box::new(move |state| {
            let _ = reply.send(work(state));
        });
        let mut envelope = Envelope::BotClient(call);
        let mut tries = 0;
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Err(Gone);
            }
            match self.sender.try_send(envelope) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return Err(Gone),
                Err(TrySendError::Full(back)) => {
                    tries += 1;
                    if tries >= Self::FULL_TRIES {
                        return Err(Gone);
                    }
                    envelope = back;
                    tokio::time::sleep(Self::FULL_STEP).await;
                }
            }
        }
        answer.await.map_err(|_| Gone)
    }
}

/// Work that may wait on the Store or on a local command, on a blocking
/// thread of the runtime.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, Gone> {
    tokio::task::spawn_blocking(work).await.map_err(|_| Gone)
}

/// The feed a stream is served under: this computer, this database's feed,
/// this start of the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    owner: BotOwner,
    feed_id: String,
    boot_id: String,
}

struct Shared {
    link: Link,
    /// The local commands, run as a window's request runs them.
    local: Arc<dyn command::LocalCommands>,
    authority: Arc<dyn BotAuthority>,
    admission: Arc<dyn ClientAdmission>,
    peers: Mutex<Peers>,
    /// One build of the feed at a time.
    starting: tokio::sync::Mutex<()>,
    /// One reconcile at a time, and when the last one finished.
    reconciling: tokio::sync::Mutex<Option<Instant>>,
    head: tokio::sync::watch::Receiver<u64>,
    feed_streams: Arc<AtomicUsize>,
    timing: Timing,
}

/// The owner's limits (contract 2.4): the devices served at once, the
/// streams of one connection, and the commands one device has in flight.
#[derive(Default)]
struct Peers {
    devices: HashMap<DeviceId, Peer>,
}

#[derive(Default)]
struct Peer {
    /// Open streams by the carrier's connection.
    connections: HashMap<u64, u32>,
    /// Commands being applied for this device, across its streams.
    inflight: u32,
}

/// One open stream's place among the limits; gives it back when dropped.
struct StreamSlot {
    shared: Arc<Shared>,
    device: DeviceId,
    connection: u64,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut peers = self.shared.peers();
        if let Some(peer) = peers.devices.get_mut(&self.device) {
            if let Some(streams) = peer.connections.get_mut(&self.connection) {
                *streams = streams.saturating_sub(1);
                if *streams == 0 {
                    peer.connections.remove(&self.connection);
                }
            }
            if peer.connections.is_empty() && peer.inflight == 0 {
                peers.devices.remove(&self.device);
            }
        }
    }
}

/// One command being applied for a device; counted until it is dropped.
struct InflightSlot {
    shared: Arc<Shared>,
    device: DeviceId,
}

impl Drop for InflightSlot {
    fn drop(&mut self) {
        let mut peers = self.shared.peers();
        if let Some(peer) = peers.devices.get_mut(&self.device) {
            peer.inflight = peer.inflight.saturating_sub(1);
            if peer.connections.is_empty() && peer.inflight == 0 {
                peers.devices.remove(&self.device);
            }
        }
    }
}

impl Shared {
    fn peers(&self) -> std::sync::MutexGuard<'_, Peers> {
        self.peers.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// A place for one more stream of `device` on `connection`, or `None`
    /// when a limit is reached: eight devices, four streams a connection.
    fn admit_stream(self: &Arc<Self>, device: DeviceId, connection: u64) -> Option<StreamSlot> {
        let mut peers = self.peers();
        if !peers.devices.contains_key(&device)
            && peers.devices.len() >= limits::MAX_PEER_DEVICES as usize
        {
            return None;
        }
        let peer = peers.devices.entry(device).or_default();
        // The carrier keeps one connection a device, and a new one
        // supersedes the old; its streams may still be ending when the new
        // one's first stream arrives. A device therefore holds at most two
        // connections' worth of streams here, however many connection ids
        // its streams carry.
        let held: u32 = peer.connections.values().sum();
        if held >= 2 * limits::MAX_STREAMS_PER_CONNECTION
            || peer
                .connections
                .get(&connection)
                .is_some_and(|streams| *streams >= limits::MAX_STREAMS_PER_CONNECTION)
        {
            return None;
        }
        *peer.connections.entry(connection).or_insert(0) += 1;
        Some(StreamSlot {
            shared: Arc::clone(self),
            device,
            connection,
        })
    }

    /// A place for one more command of `device`, or `None` when it already
    /// has four in flight.
    fn admit_command(self: &Arc<Self>, device: DeviceId) -> Option<InflightSlot> {
        let mut peers = self.peers();
        let peer = peers.devices.entry(device).or_default();
        if peer.inflight >= limits::MAX_INFLIGHT_COMMANDS {
            return None;
        }
        peer.inflight += 1;
        Some(InflightSlot {
            shared: Arc::clone(self),
            device,
        })
    }

    /// Whether `device` is, right now, a client the admission port names
    /// with `scope`. `Err` is the refusal a stream ends with when it is not:
    /// `not_own_device` for a device the port no longer names, and
    /// `unsupported_message` for one that is still a client and no longer
    /// holds the scope its stream negotiated.
    fn admitted(&self, device: &DeviceId, scope: &str) -> Result<(), Refused> {
        match self.admission.client_scopes(device) {
            None => Err(Refused::new(refusal::NOT_OWN_DEVICE)),
            Some(scopes) if !scopes.contains(scope) => {
                Err(Refused::new(refusal::UNSUPPORTED_MESSAGE))
            }
            Some(_) => Ok(()),
        }
    }

    async fn run(self: Arc<Self>, stream: BotClientStream) {
        let BotClientStream {
            hello,
            device,
            scopes: _,
            session,
            decoder,
        } = stream;
        let stream = Stream {
            shared: Arc::clone(&self),
            device,
            session,
            decoder,
        };
        // The port is read again here: what it said when the hello was read
        // is a moment older, and a device it no longer names is refused.
        let Some(scopes) = self.admission.client_scopes(&device) else {
            return stream.refuse(Refused::new(refusal::NOT_OWN_DEVICE)).await;
        };
        let supported: Vec<&str> = BUILT_CAPABILITIES
            .iter()
            .map(|capability| capability.name)
            .collect();
        let negotiated = match Negotiated::owner_side(&hello, &device, &scopes, &supported) {
            Ok(negotiated) => negotiated,
            Err(refusal) => return stream.refuse(refusal).await,
        };
        let Some(slot) = self.admit_stream(device, stream.session.connection_id) else {
            return stream.refuse(Refused::new(refusal::BUSY)).await;
        };
        let identity = match self.identity().await {
            Ok(identity) => identity,
            Err(_) => return stream.refuse(Refused::new(refusal::BOTS_UNAVAILABLE)).await,
        };
        let head = self
            .link
            .on_writer(|state| state.repository.feed_head())
            .await;
        let Ok(Ok(head)) = head else {
            return stream.refuse(Refused::new(refusal::BOTS_UNAVAILABLE)).await;
        };
        let answer = negotiated.owner_hello(
            identity.owner.device,
            identity.feed_id.clone(),
            identity.boot_id.clone(),
            head,
            Timestamp::now().millis(),
        );
        if stream.send(WireMessage::OwnerHello(answer)).await.is_err() {
            return;
        }
        tracing::debug!(
            role = hello.role.as_str(),
            "a Bot client stream was admitted"
        );
        let served = Served {
            stream,
            negotiated,
            identity,
            _slot: slot,
        };
        match hello.role {
            Role::Feed => feed::serve(served).await,
            Role::Command => command::serve(served).await,
            Role::Live => live::serve(served).await,
        }
    }

    /// The feed this run of the worker serves under, built the first time a
    /// stream asks: the start the worker recorded as it started, and the
    /// first reconcile of the markers with the Store. The Store is asked on
    /// blocking threads, never on the writer.
    async fn identity(&self) -> Result<Identity, Gone> {
        let _one = self.starting.lock().await;
        if let Some(identity) = self
            .link
            .on_writer(|state| state.bot_clients.identity())
            .await?
        {
            return Ok(identity);
        }
        let authority = Arc::clone(&self.authority);
        let owner = blocking(move || authority.owner())
            .await?
            .map_err(|_| Gone)?;
        let (pending, homes) = self
            .link
            .on_writer(move |state| {
                let boot_id = state
                    .bot_clients
                    .boot_id()
                    .ok_or(FeedRefusal::Unavailable)?;
                OwnerFeed::resume(&state.repository, owner, boot_id)
            })
            .await?
            .map_err(|_| Gone)?;
        let authority = Arc::clone(&self.authority);
        let asked: Vec<_> = homes
            .iter()
            .filter(|home| home.local)
            .map(|home| home.home)
            .collect();
        let copies = blocking(move || HomeCopies::ask(authority.as_ref(), asked)).await?;
        self.link
            .on_writer(move |state| {
                let feed = pending.reconciled(&mut state.repository, &homes, &copies)?;
                Ok::<_, FeedRefusal>(state.bot_clients.install(feed))
            })
            .await?
            .map_err(|_| Gone)
    }

    /// Compare every Bot's marker with the Store and rewrite the markers of
    /// the Bots where they differ. With `force` false it does nothing when
    /// another reconcile finished within half the reconcile interval, so
    /// several subscribed desktops whose timers fire together share one and
    /// a single desktop's own timer never skips a turn.
    async fn reconcile(&self, force: bool) -> Result<(), Gone> {
        let mut last = self.reconciling.lock().await;
        if !force && last.is_some_and(|at: Instant| at.elapsed() < self.timing.reconcile / 2) {
            return Ok(());
        }
        let homes = self
            .link
            .on_writer(|state| {
                let feed = state.bot_clients.feed().ok_or(FeedRefusal::Unavailable)?;
                feed.reconcile_homes(&state.repository)
            })
            .await?
            .map_err(|_| Gone)?;
        let authority = Arc::clone(&self.authority);
        let asked: Vec<_> = homes
            .iter()
            .filter(|home| home.local)
            .map(|home| home.home)
            .collect();
        let copies = blocking(move || HomeCopies::ask(authority.as_ref(), asked)).await?;
        let _: Reconciled = self
            .link
            .on_writer(move |state| {
                let WorkerState {
                    bot_clients,
                    repository,
                    ..
                } = state;
                let feed = bot_clients.feed_mut().ok_or(FeedRefusal::Unavailable)?;
                feed.reconcile(repository, &homes, &copies)
            })
            .await?
            .map_err(|_| Gone)?;
        *last = Some(Instant::now());
        Ok(())
    }

    /// One page above `cursor`, built on the writer when its budget allows.
    async fn page(
        &self,
        cursor: u64,
        reset: bool,
        copies: HomeCopies,
    ) -> Result<PageOutcome, Gone> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.link
            .on_writer(move |state| {
                state.bot_clients.queue_page(writer::PageRequest {
                    cursor,
                    reset,
                    copies,
                    reply,
                })
            })
            .await?;
        answer.await.map_err(|_| Gone)?.map_err(|_| Gone)
    }
}

/// What reading a stream gave.
enum Read {
    Message(WireMessage),
    /// The client closed the stream, or the carrier ended it.
    Closed,
    /// A malformed or oversized frame, or a message of an unknown type.
    Malformed,
}

/// The stream is gone; nothing more can be written to it.
struct Closed;

/// One stream and the device at its other end.
struct Stream {
    shared: Arc<Shared>,
    device: DeviceId,
    session: TransportSession,
    decoder: FrameDecoder,
}

impl Stream {
    /// The next whole message. Safe to drop while it waits: no byte is lost,
    /// the decoder keeps what arrived.
    async fn read(&mut self) -> Read {
        loop {
            match self.decoder.next() {
                Ok(Some(message)) => return Read::Message(message),
                Ok(None) => {}
                Err(_) => return Read::Malformed,
            }
            match self.session.receive.recv().await {
                Ok(Some(bytes)) => {
                    if self.decoder.push(&bytes).is_err() {
                        return Read::Malformed;
                    }
                }
                Ok(None) | Err(_) => return Read::Closed,
            }
        }
    }

    /// Write one message. A client that takes nothing for as long as it
    /// may leave a page unacknowledged is gone: the stream is not held open
    /// by a peer that stopped reading.
    async fn send(&self, message: WireMessage) -> Result<(), Closed> {
        let frame = encode(&message).map_err(|_| Closed)?;
        match tokio::time::timeout(self.shared.timing.ack, self.session.send.send(frame)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(Closed),
        }
    }

    /// Write one `refused` and end the stream. Dropping the session finishes
    /// the send side after the frame, so the client reads the refusal and
    /// then the end of the stream.
    async fn refuse(self, refusal: Refused) {
        tracing::debug!(reason = %refusal.reason, "a Bot client stream was refused");
        let _ = self.send(WireMessage::Refused(refusal)).await;
    }

    /// End the stream without a frame, as a client that broke a timeout is
    /// disconnected.
    fn disconnect(self) {
        self.session.reset(1);
    }
}

/// A stream whose hello was answered.
struct Served {
    stream: Stream,
    negotiated: Negotiated,
    identity: Identity,
    _slot: StreamSlot,
}

impl Served {
    fn shared(&self) -> &Arc<Shared> {
        &self.stream.shared
    }

    /// The checks every message gets before its role handles it (contract
    /// 4.2, checks 2 to 4): it belongs to this role and to a negotiated
    /// capability, and the device is still a client with that capability's
    /// scope. `Err` is the refusal the stream ends with.
    fn check(&self, message: &WireMessage, scope: &str) -> Result<(), Refused> {
        self.negotiated
            .check_from_client(message)
            .map_err(|violation| Refused::new(violation.refusal()))?;
        self.shared().admitted(&self.stream.device, scope)
    }

    /// Check 3 of contract 4.2: the message names this computer.
    fn names_this_owner(&self, owner_device: &DeviceId) -> Result<(), Refused> {
        if *owner_device == self.identity.owner.device {
            Ok(())
        } else {
            Err(Refused::new(refusal::WRONG_OWNER))
        }
    }
}

// Lent to the mirror's tests (`client/mirror/tests.rs`), which put a real
// owner behind a linked desktop's streams.
#[cfg(test)]
pub(in crate::worker::bots::client) mod tests;
