// SPDX-License-Identifier: MPL-2.0
//! The owner server over `TransportSession::pair()`: a linked device's
//! stream on one half, the real spawned AI worker behind the other.
//!
//! The worker runs over an in-memory `ai.db` (so the feed audit looks at
//! every commit a command makes), a Bot authority that answers where a
//! workspace is kept, an admission port a test can change, and the scripted
//! execution owner the Bot conversation tests use. No Store, provider,
//! browser, keychain, bridge or device network is opened.
//!
//! Cohorts `m8_owner_authority` ([`authority`]), `m8_owner_receipts`
//! ([`receipts`]), `m8_two_clients` ([`two_clients`]) and `m8_live_role`
//! ([`live_role`]); `m8_owner_limits` and `m8_owner_feed` hold what the
//! contract's cohort list does not name: the owner's limits and the feed
//! role's stream.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_ai_api::AiServerPort;
use koru_ai_core::bots::{
    Bot, BotAuthority, BotAuthoritySnapshot, BotCommand, BotError, BotLibraryCapture,
    BotMemoryBinding, BotOwner, BotPage, BotRequestId, BotResponse, DeviceId, HomeCopy,
    MemoryAuthorityError, VerifiedMemoryIndex,
};
use koru_ai_core::{
    AiSurface, Cancellation, ChatId, DocumentId, ProfileId, StoreRevision, WorkspaceId,
};
use koru_ai_sqlite::Repository;
use koru_ai_wire::{Failure, SendTurnRequest};
use koru_bot_client_wire::{
    Ack, Changes, ClientCommand, ClientHello, Command, FrameDecoder, OwnerHello, Receipt, Role,
    Subscribe, WireMessage, client_capabilities, encode, scope,
};
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, BotReply};
use koru_delegate_core as d;
use koru_network_edge::{DevicePrincipal, NetworkPrincipal, ProtocolToken, TransportSession};
use uuid::Uuid;

use super::command::{Applied, LocalCommands, PortCommands};
use super::*;
use crate::worker::bots::tests::chat::Native;
use crate::worker::bots::tests::{Authority, created, fixture};
use crate::worker::{AiWorker, AiWorkerClient, WorkerConfig, WorkerExit, WorkerHandle};

mod authority;
mod commands;
mod feed_role;
mod limits_cases;
mod live_role;
mod receipts;
mod two_clients;

/// The Bot authority of these tests: the shared fixture's owner and
/// workspace, Role history that can be captured, and where each workspace
/// is kept, which a test can change.
pub(in crate::worker::bots::client) struct Home {
    inner: Arc<Authority>,
    local_only: Mutex<BTreeSet<WorkspaceId>>,
}

impl Home {
    fn keep_on_this_computer(&self, workspace: WorkspaceId, kept: bool) {
        let mut local = self.local_only.lock().unwrap();
        if kept {
            local.insert(workspace);
        } else {
            local.remove(&workspace);
        }
    }
}

impl BotAuthority for Home {
    fn owner(&self) -> Result<BotOwner, BotError> {
        self.inner.owner()
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.inner.workspace(id)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.inner.page(page)
    }
    fn home_copy(&self, workspace: WorkspaceId) -> Result<HomeCopy, BotError> {
        Ok(if self.local_only.lock().unwrap().contains(&workspace) {
            HomeCopy::LocalOnly
        } else {
            HomeCopy::Copied
        })
    }
    fn pinned_document_revision(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        reader: ProfileId,
        document: DocumentId,
        version: d::DocumentVersionId,
    ) -> Result<StoreRevision, BotError> {
        self.inner
            .pinned_document_revision(owner, workspace, reader, document, version)
    }
    fn replay_memory_index(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        home: WorkspaceId,
        capture: &BotLibraryCapture,
        binding: Option<&BotMemoryBinding>,
        cancellation: &Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        self.inner
            .replay_memory_index(owner, reader, home, capture, binding, cancellation)
    }
    fn capture(
        &self,
        _owner: BotOwner,
        _workspace: WorkspaceId,
        _profile: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<d::RunContextItem>, BotError> {
        Ok(pages
            .iter()
            .map(|page| d::RunContextItem::Document {
                workspace: page.workspace,
                document: page.document,
                version: d::DocumentVersionId::new(),
            })
            .collect())
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.inner.inspect(workspaces, pages)
    }
}

/// The admission port of these tests: which devices are clients, with which
/// scopes. A test changes it while a stream is open.
#[derive(Default)]
pub(in crate::worker::bots::client) struct Port(Mutex<BTreeMap<DeviceId, BTreeSet<String>>>);

impl Port {
    pub(in crate::worker::bots::client) fn name(&self, device: DeviceId, scopes: &[&str]) {
        self.0.lock().unwrap().insert(
            device,
            scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        );
    }
    fn forget(&self, device: DeviceId) {
        self.0.lock().unwrap().remove(&device);
    }
}

impl ClientAdmission for Port {
    fn client_scopes(&self, device: &DeviceId) -> Option<BTreeSet<String>> {
        self.0.lock().unwrap().get(device).cloned()
    }
}

pub(in crate::worker::bots::client) const ALL_SCOPES: [&str; 3] =
    [scope::READ, scope::CONTROL, scope::CONFIGURE];

/// The execution owner: the scripted one of the Bot conversation tests, and
/// a Bot's own pause taken as it is sent. Anything it was not scripted for
/// is away.
pub(in crate::worker::bots::client) struct Delegate {
    pub(in crate::worker::bots::client) native: Native,
}

impl BotDelegatePort for Delegate {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            D::Barrier { barrier } => Ok(BotReply::Barrier { barrier }),
            command @ (D::Hello { .. }
            | D::SynchronizeBootBot { .. }
            | D::PendingNativeEffects { .. }
            | D::Submit { .. }
            | D::Reconcile { .. }
            | D::Observe { .. }
            | D::BeginTurn { .. }
            | D::AcknowledgeDelivery { .. }
            | D::Interrupt { .. }
            | D::Stop { .. }
            | D::TeamBarrier { .. }) => self.native.execute(command),
            _ => Err(koru_delegate_api::Failure::new(
                "fixture_away",
                "the fixture owner is not scripted for this",
                true,
            )),
        }
    }
}

/// The local commands with a gate and a script in front of the worker's own
/// port: a test can hold a command while it is being applied, or have the
/// next one answered with a failure of its choosing.
pub(in crate::worker::bots::client) struct Local {
    port: PortCommands,
    /// Hold the next `apply` until released: `(entered, release)`.
    gate: Mutex<Option<(std::sync::mpsc::SyncSender<()>, Receiver<()>)>>,
    /// Answer the next `apply` with this instead of running it.
    scripted: Mutex<Option<Failure>>,
    applied: Mutex<Vec<&'static str>>,
}

impl Local {
    fn new(port: Arc<dyn AiServerPort>) -> Self {
        Self {
            port: PortCommands(port),
            gate: Mutex::default(),
            scripted: Mutex::default(),
            applied: Mutex::default(),
        }
    }
    /// The kinds handed to the local commands so far.
    pub(in crate::worker::bots::client) fn applied(&self) -> Vec<&'static str> {
        self.applied.lock().unwrap().clone()
    }
}

impl LocalCommands for Local {
    fn apply(&self, command: ClientCommand) -> Result<Applied, Failure> {
        self.applied.lock().unwrap().push(command.kind());
        // Taken out first: the gate is not held while this command waits.
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(20)).unwrap();
        }
        if let Some(failure) = self.scripted.lock().unwrap().take() {
            return Err(failure);
        }
        self.port.apply(command)
    }
    fn read(&self, read: BotCommand) -> Result<BotResponse, Failure> {
        self.port.read(read)
    }
}

/// One computer that runs a Bot, and the owner server in front of it.
// Lent to the mirror's tests, which link a second computer to this one.
pub(in crate::worker::bots::client) struct World {
    runtime: tokio::runtime::Runtime,
    worker: Option<WorkerHandle>,
    returned: Receiver<Repository>,
    pub(in crate::worker::bots::client) server: BotOwnerServer,
    pub(in crate::worker::bots::client) local: Arc<Local>,
    pub(in crate::worker::bots::client) authority: Arc<Home>,
    pub(in crate::worker::bots::client) port: Arc<Port>,
    pub(in crate::worker::bots::client) delegate: Arc<Delegate>,
    timing: Timing,
    pub(in crate::worker::bots::client) bot: Bot,
    pub(in crate::worker::bots::client) owner: BotOwner,
}

fn spawn(
    repository: Repository,
    authority: Arc<Home>,
    port: Arc<Port>,
    delegate: Arc<Delegate>,
    timing: Timing,
) -> (
    WorkerHandle,
    Receiver<Repository>,
    BotOwnerServer,
    Arc<Local>,
) {
    let (return_tx, returned) = sync_channel(1);
    // The deterministic provider answers an ordinary chat of the person,
    // which these tests also write. A Bot's reply never comes from it: the
    // scripted execution owner's own text is what the cases read.
    let mut config =
        WorkerConfig::deterministic_mock(PathBuf::from("unused-owner-server"), "internal-debug")
            .with_bot_authority(authority)
            .with_bot_delegate(delegate);
    config.bot_admission = Some(port);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    // Bot commands run through real preparation; no session host answers
    // model discovery here.
    config.test_bot_runtime_ready = true;
    let worker = AiWorker::new(config).spawn().unwrap();
    let local = Arc::new(Local::new(Arc::new(worker.client())));
    let server = BotOwnerServer::with_local(
        worker.bot_owner.clone().expect("the worker serves Bots"),
        local.clone(),
        timing,
    );
    (worker, returned, server, local)
}

impl World {
    pub(in crate::worker::bots::client) fn new() -> Self {
        Self::with(Timing::default(), Native::default())
    }

    fn with(timing: Timing, native: Native) -> Self {
        let (mut repository, inner, create) = fixture();
        let bot = created(&mut repository, inner.clone(), create);
        let owner = inner.owner().unwrap();
        let authority = Arc::new(Home {
            inner,
            local_only: Mutex::default(),
        });
        let port = Arc::new(Port::default());
        let delegate = Arc::new(Delegate { native });
        let (worker, returned, server, local) = spawn(
            repository,
            authority.clone(),
            port.clone(),
            delegate.clone(),
            timing,
        );
        Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            worker: Some(worker),
            returned,
            server,
            local,
            authority,
            port,
            delegate,
            timing,
            bot,
            owner,
        }
    }

    pub(in crate::worker::bots::client) fn client(&self) -> AiWorkerClient {
        self.worker.as_ref().unwrap().client()
    }

    /// A device the admission port names a client with every scope.
    fn device(&self) -> DeviceId {
        let device = DeviceId::from_uuid(Uuid::new_v4());
        self.port.name(device, &ALL_SCOPES);
        device
    }

    /// Stop the AI worker and start another over the `ai.db` it left: a new
    /// start of the worker, a new owner server, the same execution owner.
    fn restart(&mut self) {
        let worker = self.worker.take().unwrap();
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        let repository = self
            .returned
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker hands its repository back");
        let (worker, returned, server, local) = spawn(
            repository,
            self.authority.clone(),
            self.port.clone(),
            self.delegate.clone(),
            self.timing,
        );
        self.worker = Some(worker);
        self.returned = returned;
        self.server = server;
        self.local = local;
    }

    /// One piece of work on the worker's writer, with its state.
    pub(in crate::worker::bots::client) fn on_writer<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut WorkerState) -> T + Send + 'static,
    ) -> T {
        let (reply, answer) = sync_channel(1);
        self.client()
            .sender
            .send(Envelope::WithStateForFixture(Box::new(move |state| {
                let _ = reply.send(work(state));
            })))
            .unwrap();
        answer.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    /// The Bot as the computer that runs it reads it.
    pub(in crate::worker::bots::client) fn read_bot(&self) -> Bot {
        match self
            .client()
            .bots(
                local_owner(),
                BotCommand::Read {
                    bot_id: self.bot.id,
                },
            )
            .unwrap()
        {
            BotResponse::Bot { bot } => bot,
            _ => panic!("bot"),
        }
    }

    /// Send one read and take its answer.
    fn query(
        &self,
        client: &mut Client,
        query: koru_bot_client_wire::ClientQuery,
    ) -> koru_bot_client_wire::Answer {
        self.runtime.block_on(async {
            client
                .write(WireMessage::Query(koru_bot_client_wire::Query {
                    query_id: "q".into(),
                    owner_device: client.owner,
                    query,
                }))
                .await;
            match client.next().await {
                Some(WireMessage::Answer(answer)) => answer,
                other => panic!("expected an answer, read {other:?}"),
            }
        })
    }

    /// Wait until no pause or resume of the Bot still waits for its
    /// execution owner, and answer the Bot.
    pub(in crate::worker::bots::client) fn settled_bot(&self) -> Bot {
        until("the Bot's change reached its owner", || {
            self.read_bot().pending_change.is_none()
        });
        self.read_bot()
    }

    /// How many turns the execution owner began.
    pub(in crate::worker::bots::client) fn begun(&self) -> usize {
        self.delegate.native.state.lock().unwrap().begin
    }

    /// Hand one stream to the owner server as the gateway's adapter does:
    /// the hello read and unanswered, the authenticated device, the port's
    /// scopes. Answers the client's half.
    fn connect(&self, device: DeviceId, hello: ClientHello, connection: u64) -> Client {
        let token = ProtocolToken::parse(koru_bot_client_wire::PROTOCOL_TOKEN).unwrap();
        let (client, mut session) = TransportSession::pair(
            principal(self.owner.device),
            principal(device),
            token,
            STREAM_FRAMES,
        );
        session.connection_id = connection;
        let scopes = self.port.client_scopes(&device).unwrap_or_default();
        let _runtime = self.runtime.enter();
        self.server.serve(BotClientStream {
            hello,
            device,
            scopes,
            session,
            decoder: FrameDecoder::new(),
        });
        Client {
            session: client,
            decoder: FrameDecoder::new(),
            owner: self.owner.device,
        }
    }

    /// Open one stream in `role` and read the owner's hello.
    fn open(&self, device: DeviceId, role: Role) -> (Client, OwnerHello) {
        self.open_on(device, role, 1)
    }

    fn open_on(&self, device: DeviceId, role: Role, connection: u64) -> (Client, OwnerHello) {
        let mut client = self.connect(device, hello(device, role), connection);
        match self.runtime.block_on(client.next()) {
            Some(WireMessage::OwnerHello(answer)) => (client, answer),
            other => panic!("expected the owner's hello, read {other:?}"),
        }
    }

    /// Open a stream that is refused, and read why.
    fn refused(&self, device: DeviceId, hello: ClientHello, connection: u64) -> String {
        let mut client = self.connect(device, hello, connection);
        self.runtime.block_on(client.refusal())
    }

    /// Send one command and read its receipt.
    fn command(&self, client: &mut Client, request_id: Uuid, command: ClientCommand) -> Receipt {
        self.runtime.block_on(async {
            client.command(request_id, command).await;
            client.receipt().await
        })
    }
}

/// The scripted execution owner, failing the first turn it is handed.
fn failing_first() -> Native {
    let mut native = Native::default();
    native.fail_first = true;
    native
}

/// The scripted execution owner, beginning a turn and never ending it.
fn never_ending() -> Native {
    let mut native = Native::default();
    native.hold_observation = true;
    native
}

/// How many frames one direction of a test stream holds.
const STREAM_FRAMES: usize = 8;

pub(in crate::worker::bots::client) fn principal(device: DeviceId) -> NetworkPrincipal {
    NetworkPrincipal::Device {
        device: DevicePrincipal {
            device_id: device,
            relationship_id: koru_device::RelationshipId::new(),
            endpoint_id: koru_device::ReplicationEndpointId::from_bytes([7; 32]),
            authorization_revision: 1,
        },
    }
}

fn hello(device: DeviceId, role: Role) -> ClientHello {
    ClientHello {
        min_version: 1,
        max_version: 1,
        capabilities: client_capabilities(),
        role,
        client_device: device,
        client_build: "test".into(),
    }
}

/// The caller a window of this computer is: what the worker's own tests use.
pub(in crate::worker::bots::client) fn local_owner() -> koru_ai_api::RequestContext {
    koru_ai_api::RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "owner-window".into(),
            scopes: vec![koru_ai_api::AiScope::Chat, koru_ai_api::AiScope::ManageBots],
        },
        trace_id: "owner-window".into(),
        deadline_millis: None,
    }
}

/// The message the scripted execution owner accepts as a task.
fn send(bot: &Bot, chat: ChatId, operation: Uuid) -> ClientCommand {
    ClientCommand::SendTurn {
        request: SendTurnRequest {
            destination_workspace: Some(bot.home_workspace),
            operation_id: operation,
            chat_id: chat,
            surface: AiSurface::Standalone,
            markdown: "CURRENT-TASK".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: Vec::new(),
            answers: None,
            skills: Vec::new(),
        },
    }
}

/// A retry of the reply `message`, as the Bot conversation tests send it.
fn retry(
    bot: &Bot,
    message: koru_ai_core::MessageId,
    operation: Uuid,
) -> koru_ai_wire::RetryTurnRequest {
    koru_ai_wire::RetryTurnRequest {
        destination_workspace: Some(bot.home_workspace),
        operation_id: operation,
        message_id: message,
        surface: AiSurface::Standalone,
        explicit_refs: Vec::new(),
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    }
}

fn pause(bot: &Bot, request: Uuid, expected_revision: u64) -> ClientCommand {
    ClientCommand::PauseBot {
        request_id: BotRequestId::from_uuid(request),
        bot_id: bot.id,
        expected_revision,
    }
}

pub(in crate::worker::bots::client) fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The linked device's half of one stream.
struct Client {
    session: TransportSession,
    decoder: FrameDecoder,
    owner: DeviceId,
}

impl Client {
    /// The next message, or `None` at the end of the stream.
    async fn next(&mut self) -> Option<WireMessage> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(message) = self.decoder.next().expect("a well-formed frame") {
                    return Some(message);
                }
                match self.session.receive.recv().await {
                    Ok(Some(bytes)) => self.decoder.push(&bytes).expect("a frame within the cap"),
                    Ok(None) | Err(_) => return None,
                }
            }
        })
        .await
        .expect("the owner answers within twenty seconds")
    }

    /// Whether nothing arrives within `within`.
    async fn silent(&mut self, within: Duration) -> bool {
        tokio::time::timeout(within, async {
            loop {
                if self.decoder.next().expect("a well-formed frame").is_some() {
                    return;
                }
                match self.session.receive.recv().await {
                    Ok(Some(bytes)) => self.decoder.push(&bytes).unwrap(),
                    Ok(None) | Err(_) => return,
                }
            }
        })
        .await
        .is_err()
    }

    async fn write(&self, message: WireMessage) {
        self.session
            .send
            .send(encode(&message).unwrap())
            .await
            .expect("the stream takes a frame");
    }

    /// Write a frame this build's types cannot say: a kind it does not
    /// know, or a member it never writes.
    async fn write_json(&self, body: serde_json::Value) {
        let body = serde_json::to_vec(&body).unwrap();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        self.session.send.send(frame).await.unwrap();
    }

    async fn command(&self, request_id: Uuid, command: ClientCommand) {
        self.write(WireMessage::Command(Command {
            request_id,
            owner_device: self.owner,
            command,
        }))
        .await;
    }

    async fn receipt(&mut self) -> Receipt {
        match self.next().await {
            Some(WireMessage::Receipt(receipt)) => receipt,
            other => panic!("expected a receipt, read {other:?}"),
        }
    }

    /// The reason of the `refused` the stream ends with.
    async fn refusal(&mut self) -> String {
        let reason = match self.next().await {
            Some(WireMessage::Refused(refused)) => refused.reason,
            other => panic!("expected a refusal, read {other:?}"),
        };
        assert!(
            self.next().await.is_none(),
            "a refused stream ends after its refusal"
        );
        reason
    }

    async fn subscribe(&self, feed_id: Option<&str>, boot_id: Option<&str>, cursor: Option<u64>) {
        self.write(WireMessage::Subscribe(Subscribe {
            owner_device: self.owner,
            feed_id: feed_id.map(str::to_owned),
            boot_id: boot_id.map(str::to_owned),
            cursor,
        }))
        .await;
    }

    /// Read pages until one brings the client up to date, acknowledging
    /// each, and answer them all.
    async fn pages(&mut self) -> Vec<Changes> {
        let mut pages = Vec::new();
        loop {
            match self.next().await {
                Some(WireMessage::Changes(page)) => {
                    self.write(WireMessage::Ack(Ack {
                        cursor: page.to_cursor,
                    }))
                    .await;
                    let caught_up = page.caught_up;
                    pages.push(page);
                    if caught_up {
                        return pages;
                    }
                }
                Some(WireMessage::Keepalive(_)) => {}
                other => panic!("expected a page, read {other:?}"),
            }
        }
    }
}

/// The failure code of a refused receipt.
fn code(receipt: &Receipt) -> &str {
    receipt
        .failure
        .as_ref()
        .map(|failure| failure.code.as_str())
        .unwrap_or_default()
}

/// Every receipt the owner holds, oldest first: its kind, its state and
/// its outcome.
pub(in crate::worker::bots::client) fn receipts(
    world: &World,
) -> Vec<(String, String, Option<String>)> {
    world.on_writer(|state| state.repository.client_receipts_for_fixture().unwrap())
}

/// The feed audit looks at every commit of this database and rolls back
/// one that changes a copied row without moving its marker. None was: every
/// write a linked device's commands caused moved its marker.
pub(in crate::worker::bots::client) fn audit_refused_nothing(world: &World) {
    let refused = world.on_writer(|state| state.repository.feed_audit_refusals());
    assert!(refused.is_empty(), "{refused:?}");
}

/// One receipt as `receipts` answers it.
fn row(kind: &str, state: &str, outcome: Option<&str>) -> (String, String, Option<String>) {
    (
        kind.to_owned(),
        state.to_owned(),
        outcome.map(str::to_owned),
    )
}
