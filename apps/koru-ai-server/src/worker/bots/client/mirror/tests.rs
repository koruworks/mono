// SPDX-License-Identifier: MPL-2.0
//! The client mirror against a real owner: two spawned AI workers, each
//! over its own in-memory `ai.db`, with every stream between them one half
//! of `TransportSession::pair()`.
//!
//! The owner is the world the owner server's own tests build
//! (`client/owner/tests.rs`): a worker that runs a Bot, its scripted
//! execution owner, and its owner server. The linked desktop ([`Desk`]) is
//! a second worker with a mirror started over a test egress ([`Dial`]) and
//! a test admission port ([`Peers`]). The egress hands each stream to the
//! owner server the way the gateway's adapter does, through a relay the
//! test can cut, refuse, read and rewrite. No Store, provider, browser,
//! keychain, bridge or device network is opened.
//!
//! Cohorts `m8_mirror_apply`, `m8_mirror_reset`, `m8_mirror_counts`,
//! `m8_mirror_never_executes`, `m8_client_commands`, `m8_routing`,
//! `m8_removal_on_revocation` and `m8_end_to_end_pair`, and
//! `m8_linked_switch` for a worker whose mirror was never started.
use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_ai_api::{AiPrincipal, AiScope, AiServerPort, RequestContext};
use koru_ai_core::bots::{
    Bot, BotAuthority, BotCommand, BotId, BotRequestId, BotResponse, DeviceId, LinkedBotsView,
    LinkedOwnerState, LinkedOwnerView,
};
use koru_ai_core::{AiSurface, ChatId, MessageState};
use koru_ai_sqlite::{ClientOutcome, MirrorCommand, MirrorCommandState, MirrorOwner, Repository};
use koru_ai_wire::{ChatTranscript, Failure, SendTurnRequest, StreamEvent, bot_linked_codes};
use koru_bot_client_wire::{
    Changes, FrameDecoder, Outcome, Record, Role, WireMessage, decode_body, encode_body,
};
use koru_network_edge::{EdgeError, NetworkEgress, NetworkTarget, ProtocolToken, TransportSession};
use uuid::Uuid;

use super::super::owner::tests::{
    ALL_SCOPES, Delegate, World, audit_refused_nothing, local_owner, principal, receipts, until,
};
use super::super::owner::{BotClientStream, BotOwnerServer};
use super::*;
use crate::worker::bots::events::recorder::{
    BotEventRecorder, RecordedBotEvent, assert_holds_none_of,
};
use crate::worker::bots::tests::chat::Native;
use crate::worker::bots::tests::{Authority, created, fixture};
use crate::worker::{AiWorker, WorkerConfig, WorkerExit, WorkerHandle};

/// The mirror's waits, short enough for a test and long enough that none
/// of them is what a case observes.
fn timing() -> Timing {
    Timing {
        backoff_first: Duration::from_millis(40),
        backoff_max: Duration::from_millis(160),
        unsupported_retry: Duration::from_millis(200),
        hello: Duration::from_secs(10),
        silence: Duration::from_secs(120),
        receipt: Duration::from_secs(15),
        settle: Duration::from_secs(10),
        call: Duration::from_secs(40),
        sweep: Duration::from_millis(150),
        delete_batch: 1,
        delete_pause: Duration::from_millis(150),
        resend_pause: Duration::from_millis(100),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A window of the linked desktop.
fn window() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "linked-window".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "linked-window".into(),
        deadline_millis: None,
    }
}

/// The admission port of these tests: which devices this computer may keep
/// a copy of. It starts told nothing.
#[derive(Default)]
struct Peers {
    now: Mutex<OwnersNow>,
    changed: Mutex<Option<tokio::sync::watch::Sender<u64>>>,
}

impl Peers {
    fn new() -> Arc<Self> {
        let peers = Self::default();
        *peers.changed.lock().unwrap() = Some(tokio::sync::watch::Sender::new(0));
        Arc::new(peers)
    }

    /// Tell the port who the owners are now.
    fn tell(&self, owners: &[DeviceId]) {
        let sequence = {
            let mut now = self.now.lock().unwrap();
            now.sequence += 1;
            now.owners = owners.iter().copied().collect();
            now.sequence
        };
        if let Some(changed) = self.changed.lock().unwrap().as_ref() {
            changed.send_replace(sequence);
        }
    }
}

impl OwnerPeers for Peers {
    fn owners(&self) -> OwnersNow {
        self.now.lock().unwrap().clone()
    }

    fn changed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.lock().unwrap().as_ref().unwrap().subscribe()
    }
}

type Rewrite = Box<dyn FnMut(WireMessage) -> WireMessage + Send>;

/// What a test sees of, and does to, the streams between the two computers.
struct Hooks {
    server: BotOwnerServer,
    owner: DeviceId,
    client: DeviceId,
    /// Whether a dial reaches the owner.
    online: AtomicBool,
    /// Every dial, answered or not.
    opened: AtomicUsize,
    relays: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// End the next command stream once its command reached the owner, so
    /// the owner applies it and its receipt never arrives.
    cut_command: AtomicBool,
    /// End every command stream before its hello is answered.
    refuse_commands: AtomicBool,
    /// The body of every command frame the linked desktop wrote.
    commands: Mutex<Vec<Vec<u8>>>,
    subscribes: AtomicUsize,
    /// Called with a command frame's body before the owner gets it.
    on_command: Mutex<Option<Box<dyn FnMut(&[u8]) + Send>>>,
    /// Rewrites what the owner writes before the linked desktop reads it.
    to_client: Mutex<Option<Rewrite>>,
}

impl Hooks {
    fn offline(&self) {
        self.online.store(false, Ordering::SeqCst);
    }

    fn online(&self) {
        self.online.store(true, Ordering::SeqCst);
    }

    /// End every open stream, as a lost connection does.
    fn sever(&self) {
        for relay in self.relays.lock().unwrap().drain(..) {
            relay.abort();
        }
    }

    fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    fn rewrite(&self, rewrite: impl FnMut(WireMessage) -> WireMessage + Send + 'static) {
        *self.to_client.lock().unwrap() = Some(Box::new(rewrite));
    }

    /// The bodies of the command frames that carried `request`.
    fn frames_of(&self, request: Uuid) -> Vec<Vec<u8>> {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|body| {
                matches!(decode_body(body), Ok(WireMessage::Command(command))
                    if command.request_id == request)
            })
            .cloned()
            .collect()
    }
}

/// The linked desktop's egress: a dial of the owner answers one half of a
/// pair and hands the other, behind a relay, to the owner server.
struct Dial(Arc<Hooks>);

impl NetworkEgress for Dial {
    fn open(
        &self,
        target: NetworkTarget,
        protocol: ProtocolToken,
    ) -> Pin<Box<dyn Future<Output = Result<TransportSession, EdgeError>> + Send + '_>> {
        let hooks = Arc::clone(&self.0);
        Box::pin(async move {
            hooks.opened.fetch_add(1, Ordering::SeqCst);
            assert_eq!(protocol.as_str(), koru_bot_client_wire::PROTOCOL_TOKEN);
            if !hooks.online.load(Ordering::SeqCst) || target.device_id() != Some(hooks.owner) {
                return Err(EdgeError::Closed);
            }
            let (client_half, near) = TransportSession::pair(
                principal(hooks.owner),
                principal(hooks.client),
                protocol.clone(),
                8,
            );
            let (far, owner_half) = TransportSession::pair(
                principal(hooks.owner),
                principal(hooks.client),
                protocol,
                8,
            );
            let relay = tokio::spawn(relay(Arc::clone(&hooks), near, far, owner_half));
            hooks.relays.lock().unwrap().push(relay);
            Ok(client_half)
        })
    }
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

/// Carry one stream between the linked desktop (`near`) and the owner
/// server (`far`, whose other half is `owner_half`). The hello is read here
/// and handed over unanswered, as the gateway's adapter does. Ending this
/// task ends the stream for both.
async fn relay(
    hooks: Arc<Hooks>,
    mut near: TransportSession,
    mut far: TransportSession,
    owner_half: TransportSession,
) {
    let mut from_client = FrameDecoder::new();
    let hello = loop {
        match from_client.next() {
            Ok(Some(WireMessage::ClientHello(hello))) => break hello,
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        match near.receive.recv().await {
            Ok(Some(bytes)) => {
                if from_client.push(&bytes).is_err() {
                    return;
                }
            }
            _ => return,
        }
    };
    if hello.role == Role::Command && hooks.refuse_commands.load(Ordering::SeqCst) {
        return;
    }
    hooks.server.serve(BotClientStream {
        hello,
        device: hooks.client,
        scopes: ALL_SCOPES
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<BTreeSet<_>>(),
        session: owner_half,
        decoder: FrameDecoder::new(),
    });
    let mut from_owner = FrameDecoder::new();
    loop {
        tokio::select! {
            bytes = near.receive.recv() => {
                let Ok(Some(bytes)) = bytes else { return };
                if from_client.push(&bytes).is_err() {
                    return;
                }
                while let Ok(Some(body)) = from_client.next_body() {
                    let kind = decode_body(&body).ok();
                    let command = matches!(kind, Some(WireMessage::Command(_)));
                    if command {
                        hooks.commands.lock().unwrap().push(body.clone());
                        if let Some(hook) = hooks.on_command.lock().unwrap().as_mut() {
                            hook(&body);
                        }
                    }
                    if matches!(kind, Some(WireMessage::Subscribe(_))) {
                        hooks.subscribes.fetch_add(1, Ordering::SeqCst);
                    }
                    if far.send.send(framed(&body)).await.is_err() {
                        return;
                    }
                    if command && hooks.cut_command.swap(false, Ordering::SeqCst) {
                        return;
                    }
                }
            }
            bytes = far.receive.recv() => {
                let Ok(Some(bytes)) = bytes else { return };
                if from_owner.push(&bytes).is_err() {
                    return;
                }
                while let Ok(Some(body)) = from_owner.next_body() {
                    let body = {
                        let mut rewrite = hooks.to_client.lock().unwrap();
                        match (rewrite.as_mut(), decode_body(&body)) {
                            (Some(rewrite), Ok(message)) => {
                                encode_body(&rewrite(message)).unwrap()
                            }
                            _ => body,
                        }
                    };
                    if near.send.send(framed(&body)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// What a linked desktop is made of, kept across a restart of its worker.
struct Kept {
    repository: Repository,
    authority: Arc<Authority>,
    delegate: Option<Arc<Delegate>>,
    local: Option<Bot>,
}

/// A linked desktop: a second AI worker, its mirror, and the port a window
/// of it reads through.
struct Desk {
    worker: Option<WorkerHandle>,
    returned: Receiver<Repository>,
    routed: koru_ai_client::Client,
    peers: Arc<Peers>,
    hooks: Arc<Hooks>,
    device: DeviceId,
    authority: Arc<Authority>,
    delegate: Option<Arc<Delegate>>,
    /// A Bot this desktop runs itself.
    local: Option<Bot>,
    /// Every Bot event this start of the desktop's worker published.
    events: Arc<BotEventRecorder>,
}

/// A worker for a linked desktop over `kept`, with no mirror started.
fn spawn(
    kept: &mut Option<Repository>,
    authority: &Arc<Authority>,
    delegate: &Option<Arc<Delegate>>,
) -> (WorkerHandle, Receiver<Repository>) {
    spawn_recorded(kept, authority, delegate, BotEventRecorder::new())
}

/// [`spawn`], publishing the worker's Bot events to `events`.
fn spawn_recorded(
    kept: &mut Option<Repository>,
    authority: &Arc<Authority>,
    delegate: &Option<Arc<Delegate>>,
    events: Arc<BotEventRecorder>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config =
        WorkerConfig::deterministic_mock(PathBuf::from("unused-mirror"), "internal-debug")
            .with_bot_events(events)
            .with_bot_authority(authority.clone());
    if let Some(delegate) = delegate {
        config = config.with_bot_delegate(delegate.clone());
    }
    config.test_repository = kept.take();
    config.test_repository_return = Some(return_tx);
    config.test_bot_runtime_ready = true;
    (AiWorker::new(config).spawn().unwrap(), returned)
}

impl Desk {
    /// A linked desktop that also runs a Bot of its own.
    fn new(runtime: &tokio::runtime::Runtime, world: &World) -> Self {
        let (mut repository, authority, create) = fixture();
        let local = created(&mut repository, authority.clone(), create);
        Self::start(
            runtime,
            world,
            Kept {
                repository,
                authority,
                delegate: Some(Arc::new(Delegate {
                    native: Native::default(),
                })),
                local: Some(local),
            },
        )
    }

    /// A linked desktop with no Bot of its own and nothing that could run
    /// one: no execution owner at all.
    fn plain(runtime: &tokio::runtime::Runtime, world: &World) -> Self {
        let (repository, authority, _) = fixture();
        Self::start(
            runtime,
            world,
            Kept {
                repository,
                authority,
                delegate: None,
                local: None,
            },
        )
    }

    /// Start a worker over `kept` and a mirror whose port was told nothing.
    fn start(runtime: &tokio::runtime::Runtime, world: &World, kept: Kept) -> Self {
        let Kept {
            repository,
            authority,
            delegate,
            local,
        } = kept;
        let device = authority.owner().unwrap().device;
        assert_ne!(device, world.owner.device);
        let events = BotEventRecorder::new();
        let (worker, returned) =
            spawn_recorded(&mut Some(repository), &authority, &delegate, events.clone());
        let hooks = Arc::new(Hooks {
            server: world.server.clone(),
            owner: world.owner.device,
            client: device,
            online: AtomicBool::new(true),
            opened: AtomicUsize::new(0),
            relays: Mutex::default(),
            cut_command: AtomicBool::new(false),
            refuse_commands: AtomicBool::new(false),
            commands: Mutex::default(),
            subscribes: AtomicUsize::new(0),
            on_command: Mutex::default(),
            to_client: Mutex::default(),
        });
        let peers = Peers::new();
        let mirror = BotMirror::start(
            worker
                .bot_mirror_parts
                .clone()
                .expect("a worker with a Bot authority can keep copies"),
            Arc::new(Dial(Arc::clone(&hooks))),
            peers.clone(),
            timing(),
            runtime.handle().clone(),
        );
        let routed = koru_ai_client::Client::routed(Arc::new(worker.client()), mirror.linked());
        Self {
            worker: Some(worker),
            returned,
            routed,
            peers,
            hooks,
            device,
            authority,
            delegate,
            local,
            events,
        }
    }

    /// Stop the worker and answer what it leaves: its `ai.db`.
    fn stop(mut self) -> Kept {
        let worker = self.worker.take().unwrap();
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
        Kept {
            repository: self
                .returned
                .recv_timeout(Duration::from_secs(10))
                .expect("the worker hands its repository back"),
            authority: self.authority.clone(),
            delegate: self.delegate.clone(),
            local: self.local.clone(),
        }
    }

    /// One piece of work on the linked desktop's writer.
    fn on_writer<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut WorkerState) -> T + Send + 'static,
    ) -> T {
        let (reply, answer) = sync_channel(1);
        self.worker
            .as_ref()
            .unwrap()
            .client()
            .sender
            .send(Envelope::WithStateForFixture(Box::new(move |state| {
                let _ = reply.send(work(state));
            })))
            .unwrap();
        answer.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    /// The rows of the six client tables: owners, Bots, conversations,
    /// messages, team, commands.
    fn rows(&self) -> [i64; 6] {
        self.on_writer(|state| state.repository.mirror_row_counts_for_fixture())
    }

    fn owners(&self) -> Vec<LinkedOwnerView> {
        match self
            .routed
            .bots(window(), BotCommand::ListLinkedOwners {})
            .unwrap()
        {
            BotResponse::LinkedOwners { owners } => owners,
            _ => panic!("linked owners"),
        }
    }

    fn state(&self, owner: DeviceId) -> Option<LinkedOwnerState> {
        self.owners()
            .into_iter()
            .find(|view| view.owner_device == owner)
            .map(|view| view.state)
    }

    fn linked(&self, owner: DeviceId) -> LinkedBotsView {
        match self
            .routed
            .bots(
                window(),
                BotCommand::ListLinkedBots {
                    owner_device: owner,
                    workspace: None,
                },
            )
            .unwrap()
        {
            BotResponse::LinkedBots { linked } => linked,
            _ => panic!("linked Bots"),
        }
    }

    /// A Bot as the routing port reads it.
    fn read(&self, bot: BotId) -> Result<Bot, Failure> {
        match self
            .routed
            .bots(window(), BotCommand::Read { bot_id: bot })?
        {
            BotResponse::Bot { bot } => Ok(bot),
            _ => panic!("a Bot"),
        }
    }

    fn transcript(&self, chat: ChatId) -> ChatTranscript {
        self.routed.read_chat(window(), chat).unwrap()
    }

    fn row(&self, owner: DeviceId) -> MirrorOwner {
        self.on_writer(move |state| state.repository.mirror_owner(owner).unwrap().unwrap())
    }

    fn command(&self, owner: DeviceId, request: Uuid) -> Option<MirrorCommand> {
        self.on_writer(move |state| state.repository.mirror_command(owner, request).unwrap())
    }

    /// Link this desktop to the owner: the owner's port names it a client,
    /// its own port names the owner, and the copy is brought up to date.
    fn link(&self, world: &World) {
        world.port.name(self.device, &ALL_SCOPES);
        self.peers.tell(&[world.owner.device]);
        self.current(world);
    }

    fn current(&self, world: &World) {
        let owner = world.owner.device;
        until("the copy is up to date", || {
            self.state(owner) == Some(LinkedOwnerState::Current)
        });
    }
}

fn delivery(failure: &Failure) -> &str {
    failure
        .details
        .get("delivery")
        .and_then(|delivery| delivery.as_str())
        .unwrap_or_default()
}

/// The message the scripted execution owner accepts as a task.
fn task(bot: &Bot, chat: ChatId, operation: Uuid) -> SendTurnRequest {
    SendTurnRequest {
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
    }
}

/// A window of the owner pauses its Bot, or resumes it, and the change has
/// reached its execution owner.
fn owner_toggles(world: &World) -> Bot {
    let bot = world.settled_bot();
    let request_id = BotRequestId::mint();
    let command = if bot.paused {
        BotCommand::Resume {
            request_id,
            bot_id: bot.id,
            expected_revision: bot.revision,
        }
    } else {
        BotCommand::Pause {
            request_id,
            bot_id: bot.id,
            expected_revision: bot.revision,
        }
    };
    world.client().bots(local_owner(), command).unwrap();
    world.settled_bot()
}

fn pause(bot: &Bot, request: BotRequestId) -> BotCommand {
    BotCommand::Pause {
        request_id: request,
        bot_id: bot.id,
        expected_revision: bot.revision,
    }
}

fn resume(bot: &Bot, request: BotRequestId) -> BotCommand {
    BotCommand::Resume {
        request_id: request,
        bot_id: bot.id,
        expected_revision: bot.revision,
    }
}

/// `m8_linked_switch`: a worker whose mirror was never started keeps no
/// copy. It answers the two linked reads "no linked Bots", to a window
/// only, hands the routing port nothing, and its six client tables hold
/// no row.
#[test]
fn m8_linked_switch_a_worker_without_a_mirror_answers_no_linked_bots_and_writes_nothing() {
    let (repository, authority, _) = fixture();
    let (worker, _returned) = spawn(&mut Some(repository), &authority, &None);
    assert!(worker.linked_bots().is_none());
    let client = worker.client();
    assert_eq!(
        client
            .bots(window(), BotCommand::ListLinkedOwners {})
            .unwrap(),
        BotResponse::LinkedOwners { owners: Vec::new() }
    );
    let other = DeviceId::from_uuid(Uuid::new_v4());
    assert_eq!(
        client
            .bots(
                window(),
                BotCommand::ListLinkedBots {
                    owner_device: other,
                    workspace: None
                }
            )
            .unwrap(),
        BotResponse::LinkedBots {
            linked: LinkedBotsView {
                owner_device: other,
                state: LinkedOwnerState::Never,
                last_confirmed_at_ms: None,
                bots: Vec::new(),
            }
        }
    );
    // Not for a caller that is not a window of this person.
    let mut stranger = window();
    stranger.principal.scopes = vec![AiScope::Chat];
    assert_eq!(
        client
            .bots(stranger, BotCommand::ListLinkedOwners {})
            .unwrap_err()
            .code,
        "forbidden"
    );
    let (reply, answer) = sync_channel(1);
    client
        .sender
        .send(Envelope::WithStateForFixture(Box::new(move |state| {
            let _ = reply.send(state.repository.mirror_row_counts_for_fixture());
        })))
        .unwrap();
    assert_eq!(
        answer.recv_timeout(Duration::from_secs(10)).unwrap(),
        [0; 6]
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

/// `m8_mirror_apply`: a linked desktop follows the owner's feed. The copy
/// holds the owner's Bot as the owner's own read answers it, its main
/// conversation with the label filled from the copied Bot, and it follows
/// a change the owner makes afterwards, page by page, with no reset.
#[test]
fn m8_mirror_apply_the_copy_holds_what_the_owner_shows_and_follows_it() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    // Told nothing yet: no owner, no dial.
    assert!(desk.owners().is_empty());
    assert_eq!(desk.hooks.opened(), 0);
    desk.link(&world);

    let owners = desk.owners();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].owner_device, owner);
    assert_eq!(owners[0].state, LinkedOwnerState::Current);
    assert_eq!(owners[0].withheld_bots, 0);
    assert!(owners[0].last_confirmed_at_ms.is_some());
    let view = desk.linked(owner);
    assert_eq!(view.state, LinkedOwnerState::Current);
    assert_eq!(view.bots.len(), 1);
    let theirs = world.read_bot();
    let copy = &view.bots[0];
    assert_eq!(
        (copy.id, copy.owner_device, copy.revision, &copy.config),
        (theirs.id, owner, theirs.revision, &theirs.config)
    );
    // Only the Bots of the workspace asked for.
    let elsewhere = match desk
        .routed
        .bots(
            window(),
            BotCommand::ListLinkedBots {
                owner_device: owner,
                workspace: Some(koru_ai_core::WorkspaceId::new()),
            },
        )
        .unwrap()
    {
        BotResponse::LinkedBots { linked } => linked,
        _ => panic!("linked Bots"),
    };
    assert!(elsewhere.bots.is_empty() && elsewhere.state == LinkedOwnerState::Current);
    // Routed by its id, the Bot reads from the copy.
    assert_eq!(desk.read(theirs.id).unwrap().id, theirs.id);
    let BotResponse::Chats { page } = desk
        .routed
        .bots(
            window(),
            BotCommand::ListBotChats {
                bot_id: theirs.id,
                cursor: None,
                limit: 50,
            },
        )
        .unwrap()
    else {
        panic!("conversations")
    };
    assert_eq!(page.chats.len(), 1);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.chats[0].chat.id, theirs.main_chat);
    assert_eq!(page.chats[0].label.bot_name, theirs.config.name);
    let transcript = desk.transcript(theirs.main_chat);
    assert_eq!(
        transcript.chat.bot.as_ref().map(|label| label.bot_id),
        Some(theirs.id)
    );
    assert!(transcript.messages.is_empty());
    assert_eq!(desk.rows()[..4], [1, 1, 1, 0]);

    // The owner changes the Bot: the copy follows on the same connection.
    let generation = desk.row(owner).generation;
    let dials = desk.hooks.opened();
    let paused = owner_toggles(&world);
    assert!(paused.paused);
    until("the copy shows the pause", || {
        desk.read(theirs.id).is_ok_and(|bot| bot.paused)
    });
    assert_eq!(desk.row(owner).generation, generation, "no reset");
    assert_eq!(desk.hooks.opened(), dials, "no redial");
    assert_eq!(desk.hooks.subscribes.load(Ordering::SeqCst), 1);
    audit_refused_nothing(&world);
}

/// `m8_mirror_apply`: each page check of contract 4.6 and 5.2. A page that
/// is not the next one, that comes from another feed or start, whose record
/// is not its entry's, that names another computer as its owner, or that
/// claims a Bot of this desktop is not applied: the copy and its cursor are
/// what they were, the stream is closed, and the next connection, served
/// honestly, brings the copy to what the owner shows.
#[test]
fn m8_mirror_apply_a_page_that_fails_a_check_is_not_applied_and_the_stream_is_closed() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    let local = desk.local.clone().unwrap().id;
    desk.link(&world);
    let bot = world.bot.id;
    let stranger = DeviceId::from_uuid(Uuid::new_v4());
    type Tamper = Box<dyn Fn(&mut Changes) + Send + Sync>;
    fn of_bot(page: &Changes) -> usize {
        page.entries
            .iter()
            .position(|entry| entry.kind == "bot")
            .expect("the page carries the Bot")
    }
    let tampers: Vec<(&str, Tamper)> = vec![
        (
            "a page that does not start at the cursor",
            Box::new(|page: &mut Changes| page.from_cursor += 1),
        ),
        (
            "a page of another feed",
            Box::new(|page: &mut Changes| page.feed_id = "f".repeat(32)),
        ),
        (
            "a page of another start",
            Box::new(|page: &mut Changes| page.boot_id = "b".repeat(32)),
        ),
        (
            "a seq past the page",
            Box::new(|page: &mut Changes| page.entries[0].seq = page.to_cursor + 1),
        ),
        (
            "a record that is not its entry's",
            Box::new(move |page: &mut Changes| {
                let index = of_bot(page);
                page.entries[index].id = BotId::mint().to_string();
            }),
        ),
        (
            "a Bot that names another computer",
            Box::new(move |page: &mut Changes| {
                let index = of_bot(page);
                if let Some(Record::Bot(record)) = &mut page.entries[index].record {
                    record.bot.owner_device = stranger;
                }
            }),
        ),
        (
            "a Bot of this desktop",
            Box::new(move |page: &mut Changes| {
                let index = of_bot(page);
                page.entries[index].id = local.to_string();
                if let Some(Record::Bot(record)) = &mut page.entries[index].record {
                    record.bot.id = local;
                }
            }),
        ),
    ];
    for (what, tamper) in tampers {
        let before = desk.row(owner);
        let shown = desk.read(bot).unwrap();
        let rows = desk.rows();
        // The next page that carries the Bot is rewritten, once. After the
        // stream is closed nothing redials until the copy was looked at.
        desk.hooks.offline();
        let mut done = false;
        desk.hooks.rewrite(move |mut message| {
            if let WireMessage::Changes(page) = &mut message {
                if !done && page.entries.iter().any(|entry| entry.kind == "bot") {
                    done = true;
                    tamper(page);
                }
            }
            message
        });
        let theirs = owner_toggles(&world);
        until(what, || desk.state(owner) == Some(LinkedOwnerState::Stale));
        let after = desk.row(owner);
        assert_eq!(
            (after.cursor, after.generation, after.staging),
            (before.cursor, before.generation, None),
            "{what}"
        );
        assert_eq!(desk.rows(), rows, "{what}");
        assert_eq!(desk.read(bot).unwrap(), shown, "{what}");
        // The Bot of this desktop is still this desktop's.
        assert_eq!(
            desk.read(local).unwrap().owner_device,
            desk.device,
            "{what}"
        );
        desk.hooks.online();
        until(what, || {
            desk.state(owner) == Some(LinkedOwnerState::Current)
                && desk.read(bot).is_ok_and(|bot| bot.paused == theirs.paused)
        });
        assert_eq!(desk.row(owner).generation, before.generation, "{what}");
    }
    audit_refused_nothing(&world);
}

/// `m8_mirror_reset` and `m8_mirror_counts`: a copy whose counts are not
/// the counts the owner sends asks for one reset, which the owner serves
/// from 0; the staged copy replaces the old one. It asks once per
/// connection: counts that still differ afterwards are not asked about
/// again, and the copy keeps following.
#[test]
fn m8_mirror_counts_a_mismatch_asks_for_one_reset_per_connection() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    // Every page that brings the copy up to date claims one message more
    // than the owner holds.
    desk.hooks.rewrite(|mut message| {
        if let WireMessage::Changes(page) = &mut message {
            if let Some(live) = &mut page.live {
                live.messages += 1;
            }
        }
        message
    });
    world.port.name(desk.device, &ALL_SCOPES);
    desk.peers.tell(&[owner]);
    // The first subscription is a reset (generation 1). Its counts differ:
    // one more reset (generation 2).
    until("the reset the copy asked for was applied", || {
        desk.on_writer(move |state| state.repository.mirror_owner(owner).unwrap())
            .is_some_and(|row| row.generation == 2 && row.state == LinkedOwnerState::Current)
    });
    assert_eq!(desk.hooks.subscribes.load(Ordering::SeqCst), 2);
    assert_eq!(desk.rows()[..4], [1, 1, 1, 0], "only the new generation");
    assert_eq!(desk.linked(owner).bots.len(), 1);
    // The counts still differ. The copy does not ask again and follows.
    let theirs = owner_toggles(&world);
    until("the copy follows after its one reset", || {
        desk.read(world.bot.id)
            .is_ok_and(|bot| bot.paused == theirs.paused)
    });
    assert_eq!(desk.hooks.subscribes.load(Ordering::SeqCst), 2);
    assert_eq!(desk.row(owner).generation, 2);
    assert_eq!(desk.hooks.opened(), 1, "one connection");
}

/// `m8_mirror_never_executes`: a linked desktop receives a Bot, a message
/// sent on the owner, its running reply and its end, and a pause. No row of
/// any table but the six client tables is written on the desktop, by a
/// worker that has nothing that could run a Bot, and the feed this desktop
/// would itself serve does not move.
#[test]
fn m8_mirror_never_executes_applying_pages_starts_nothing_and_writes_no_canonical_table() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::plain(&runtime, &world);
    let owner = world.owner.device;
    let before = desk.on_writer(|state| state.repository.canonical_rows_for_fixture());
    assert!(
        before
            .iter()
            .any(|(table, _)| table == "inference_attempts")
    );
    let head = desk.on_writer(|state| state.repository.feed_head().unwrap());
    desk.link(&world);
    // A window of the owner sends the Bot a task.
    let chat = world.bot.main_chat;
    let _reply = world
        .client()
        .send_turn(local_owner(), task(&world.bot, chat, Uuid::new_v4()))
        .unwrap();
    until("the reply and its end reached the copy", || {
        let messages = desk.transcript(chat).messages;
        messages.len() == 2 && messages.iter().all(|message| message.state.is_terminal())
    });
    assert_eq!(world.begun(), 1, "the owner ran it, once");
    let theirs = owner_toggles(&world);
    until("the pause reached the copy", || {
        desk.read(world.bot.id)
            .is_ok_and(|bot| bot.paused == theirs.paused)
    });
    assert_eq!(desk.rows()[..4], [1, 1, 1, 2]);
    assert_eq!(
        desk.on_writer(|state| state.repository.canonical_rows_for_fixture()),
        before
    );
    assert_eq!(
        desk.on_writer(|state| state.repository.feed_head().unwrap()),
        head
    );
    assert!(
        desk.on_writer(|state| state.repository.feed_audit_refusals())
            .is_empty()
    );
    // The desktop's own lists hold nothing of the copy.
    assert!(
        desk.routed
            .list_chats(window(), None)
            .unwrap()
            .chats
            .is_empty()
    );
    assert_eq!(desk.state(owner), Some(LinkedOwnerState::Current));
}

/// `m8_client_commands`: the command's row exists, as `sent` and with the
/// very bytes of its frame, before those bytes reach the owner. The
/// owner's receipt then settles the row, and the caller is answered when
/// the copy shows what the command did.
#[test]
fn m8_client_commands_the_row_is_written_before_the_bytes_leave() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    desk.link(&world);
    let copy = desk.read(world.bot.id).unwrap();
    let request = BotRequestId::mint();
    // What the desktop holds at the moment the frame passes the wire.
    let seen: Arc<Mutex<Vec<(MirrorCommandState, bool)>>> = Arc::default();
    let (sender, record) = (desk.worker.as_ref().unwrap().client().sender, seen.clone());
    *desk.hooks.on_command.lock().unwrap() = Some(Box::new(move |body| {
        let body = String::from_utf8(body.to_vec()).unwrap();
        let (reply, answer) = sync_channel(1);
        sender
            .send(Envelope::WithStateForFixture(Box::new(move |state| {
                let _ = reply.send(
                    state
                        .repository
                        .mirror_command(owner, request.as_uuid())
                        .unwrap(),
                );
            })))
            .unwrap();
        let row = answer
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("the row is written before the frame");
        record.lock().unwrap().push((
            row.state,
            row.command_json.as_deref() == Some(body.as_str()),
        ));
    }));
    assert!(desk.command(owner, request.as_uuid()).is_none());
    let answered = desk.routed.bots(window(), pause(&copy, request)).unwrap();
    assert!(
        matches!(answered, BotResponse::Operation { .. }),
        "{answered:?}"
    );
    assert_eq!(*seen.lock().unwrap(), [(MirrorCommandState::Sent, true)]);
    let row = desk.command(owner, request.as_uuid()).unwrap();
    assert_eq!(row.state, MirrorCommandState::Receipted);
    assert_eq!(row.kind, "pause_bot");
    assert_eq!(row.bot_id, Some(world.bot.id));
    assert!(matches!(
        row.outcome,
        Some(ClientOutcome::Completed | ClientOutcome::Accepted)
    ));
    assert!(row.command_json.is_none(), "the bytes go with the receipt");
    // Shown as done: the receipt arrived and the copy reached its cursor.
    assert!(desk.row(owner).cursor >= row.feed_cursor.unwrap());
    assert!(desk.read(world.bot.id).unwrap().paused);
    assert!(world.settled_bot().paused);
    // A refusal is the owner's own answer, settled too: the copy's
    // revision is behind now.
    let stale = BotRequestId::mint();
    let refused = desk
        .routed
        .bots(window(), resume(&copy, stale))
        .unwrap_err();
    assert_eq!(refused.code, "revision_conflict");
    let row = desk.command(owner, stale.as_uuid()).unwrap();
    assert_eq!(
        (row.state, row.outcome, row.failure_code.as_deref()),
        (
            MirrorCommandState::Receipted,
            Some(ClientOutcome::Refused),
            Some("revision_conflict")
        )
    );
    // A change the owner does not take from a linked computer never
    // leaves: nothing is recorded and no frame is written.
    let frames = desk.hooks.commands.lock().unwrap().len();
    let archived = desk
        .routed
        .bots(
            window(),
            BotCommand::Archive {
                request_id: BotRequestId::mint(),
                bot_id: world.bot.id,
                expected_revision: 1,
            },
        )
        .unwrap_err();
    assert_eq!(archived.code, bot_linked_codes::EXECUTING_COMPUTER_ONLY);
    assert_eq!(desk.hooks.commands.lock().unwrap().len(), frames);
    assert_eq!(desk.rows()[5], 2);
    audit_refused_nothing(&world);
}

/// `m8_client_commands`: the other changes a linked desktop may make are
/// made on the owner and then read from the copy: a setting of the Bot,
/// and another conversation. A change of the working folder is the
/// owner's own refusal, and a read the owner answers itself is the answer
/// a window of the owner gets.
#[test]
fn m8_client_commands_a_setting_and_a_new_conversation_are_made_on_the_owner() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    desk.link(&world);
    let copy = desk.read(world.bot.id).unwrap();
    let update = |bot: &Bot, config: koru_ai_core::bots::BotConfig, request: BotRequestId| {
        BotCommand::Update {
            request_id: request,
            bot_id: bot.id,
            expected_revision: bot.revision,
            config,
        }
    };

    // The name.
    let mut config = copy.config.clone();
    config.name = "Renamed from a linked desktop".into();
    let renamed = BotRequestId::mint();
    desk.routed
        .bots(window(), update(&copy, config.clone(), renamed))
        .unwrap();
    assert_eq!(world.read_bot().config.name, config.name);
    let copy = desk.read(world.bot.id).unwrap();
    assert_eq!(
        copy.config.name, config.name,
        "the copy shows it when answered"
    );

    // The working folder: only the computer that runs the Bot changes it.
    let mut moved = copy.config.clone();
    moved.working_folder = if cfg!(windows) {
        "D:/elsewhere"
    } else {
        "/elsewhere"
    }
    .into();
    let refused = desk
        .routed
        .bots(window(), update(&copy, moved, BotRequestId::mint()))
        .unwrap_err();
    assert_eq!(refused.code, bot_linked_codes::EXECUTING_COMPUTER_ONLY);
    assert_eq!(
        world.read_bot().config.working_folder,
        copy.config.working_folder
    );

    // Another conversation.
    let created = desk
        .routed
        .bots(
            window(),
            BotCommand::CreateChat {
                request_id: BotRequestId::mint(),
                bot_id: copy.id,
            },
        )
        .unwrap();
    assert!(matches!(created, BotResponse::Chat { .. }), "{created:?}");
    until("the copy lists the new conversation", || {
        matches!(
            desk.routed.bots(
                window(),
                BotCommand::ListBotChats {
                    bot_id: copy.id,
                    cursor: None,
                    limit: 10,
                },
            ),
            Ok(BotResponse::Chats { page }) if page.chats.len() == 2
                && page.chats[0].chat.id == copy.main_chat
        )
    });
    // One conversation a page: the cursor leads to the second.
    let BotResponse::Chats { page } = desk
        .routed
        .bots(
            window(),
            BotCommand::ListBotChats {
                bot_id: copy.id,
                cursor: None,
                limit: 1,
            },
        )
        .unwrap()
    else {
        panic!("conversations")
    };
    assert_eq!(page.chats.len(), 1);
    let BotResponse::Chats { page: second } = desk
        .routed
        .bots(
            window(),
            BotCommand::ListBotChats {
                bot_id: copy.id,
                cursor: page.next_cursor.clone(),
                limit: 1,
            },
        )
        .unwrap()
    else {
        panic!("conversations")
    };
    assert!(page.next_cursor.is_some() && second.next_cursor.is_none());
    assert_eq!(second.chats.len(), 1);
    assert_ne!(second.chats[0].chat.id, copy.main_chat);

    // A read the owner answers itself: what its own window is answered.
    let read = BotCommand::ReadOperation {
        request_id: renamed,
        bot_id: copy.id,
    };
    let theirs = world
        .client()
        .bots(local_owner(), read.clone())
        .map_err(|failure| failure.code);
    let ours = desk
        .routed
        .bots(window(), read.clone())
        .map_err(|failure| failure.code);
    assert_eq!(ours, theirs);
    // It is asked of the owner: with the owner away it is not answered.
    desk.hooks.offline();
    desk.hooks.sever();
    until("the copy is stale", || {
        desk.state(world.owner.device) == Some(LinkedOwnerState::Stale)
    });
    let away = desk.routed.bots(window(), read).unwrap_err();
    assert_eq!(delivery(&away), bot_linked_codes::DELIVERY_NOT_SENT);
    audit_refused_nothing(&world);
}

/// `m8_client_commands`: a stream that ends after the command was written
/// and before its receipt arrived leaves the row `unconfirmed`, and the
/// caller is told so. On the next connection the mirror writes the same
/// bytes again; the owner, which applied the command once, answers from
/// its receipt. An answer of `unknown` is final: that request is never
/// written again, by the mirror or for the caller.
#[test]
fn m8_client_commands_a_lost_receipt_is_unconfirmed_and_asked_again_with_the_same_bytes() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    desk.link(&world);
    let copy = desk.read(world.bot.id).unwrap();

    // The receipt of a pause is lost.
    let lost = BotRequestId::mint();
    desk.hooks.cut_command.store(true, Ordering::SeqCst);
    let failure = desk.routed.bots(window(), pause(&copy, lost)).unwrap_err();
    assert_eq!(failure.code, bot_linked_codes::BOT_OWNER_UNREACHABLE);
    assert_eq!(delivery(&failure), bot_linked_codes::DELIVERY_NOT_CONFIRMED);
    assert!(!failure.retryable, "the caller does not send it again");
    let row = desk.command(owner, lost.as_uuid()).unwrap();
    assert_eq!(row.state, MirrorCommandState::Unconfirmed);
    assert!(row.command_json.is_some());
    // The owner took it, once.
    until("the owner applied the pause", || world.settled_bot().paused);
    // A repeat by the caller is answered from the row. Nothing is written.
    let again = desk.routed.bots(window(), pause(&copy, lost)).unwrap_err();
    assert_eq!(delivery(&again), bot_linked_codes::DELIVERY_NOT_CONFIRMED);
    assert_eq!(desk.hooks.frames_of(lost.as_uuid()).len(), 1);
    // The same id with other content is a conflict, and is not written.
    let conflict = desk.routed.bots(window(), resume(&copy, lost)).unwrap_err();
    assert_eq!(conflict.code, "request_conflict");

    // The connection is lost and made again: the mirror asks.
    desk.hooks.sever();
    until("the pause was confirmed", || {
        desk.command(owner, lost.as_uuid())
            .is_some_and(|row| row.state == MirrorCommandState::Receipted)
    });
    let row = desk.command(owner, lost.as_uuid()).unwrap();
    assert!(matches!(
        row.outcome,
        Some(ClientOutcome::Completed | ClientOutcome::Accepted)
    ));
    let frames = desk.hooks.frames_of(lost.as_uuid());
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], frames[1], "the re-send is byte-identical");
    // One effect on the owner: one receipt, and the local command ran once.
    assert_eq!(
        receipts(&world)
            .iter()
            .filter(|(kind, _, _)| kind == "pause_bot")
            .count(),
        1
    );
    assert_eq!(world.local.applied(), ["pause_bot"]);
    // The caller's repeat now learns it was answered, and how.
    let answered = desk.routed.bots(window(), pause(&copy, lost)).unwrap_err();
    assert_eq!(
        answered.code,
        bot_linked_codes::BOT_REQUEST_ALREADY_ANSWERED
    );
    assert!(answered.details.contains_key("outcome"));
    assert_eq!(desk.hooks.frames_of(lost.as_uuid()).len(), 2);

    // The receipt of a resume is lost, and when asked again the owner
    // answers that it cannot tell.
    desk.current(&world);
    until("the copy shows the pause", || {
        desk.read(world.bot.id).is_ok_and(|bot| bot.paused)
    });
    let copy = desk.read(world.bot.id).unwrap();
    let unknown = BotRequestId::mint();
    desk.hooks.cut_command.store(true, Ordering::SeqCst);
    let failure = desk
        .routed
        .bots(window(), resume(&copy, unknown))
        .unwrap_err();
    assert_eq!(delivery(&failure), bot_linked_codes::DELIVERY_NOT_CONFIRMED);
    desk.hooks.rewrite(|mut message| {
        if let WireMessage::Receipt(receipt) = &mut message {
            receipt.outcome = Outcome::Unknown;
            receipt.response = None;
            receipt.failure = None;
            receipt.feed_cursor = None;
        }
        message
    });
    desk.hooks.sever();
    until("the resume was answered", || {
        desk.command(owner, unknown.as_uuid())
            .is_some_and(|row| row.state == MirrorCommandState::Receipted)
    });
    let row = desk.command(owner, unknown.as_uuid()).unwrap();
    assert_eq!(row.outcome, Some(ClientOutcome::Unknown));
    assert_eq!(desk.hooks.frames_of(unknown.as_uuid()).len(), 2);
    // Final: another connection does not write it again, and the caller's
    // repeat is told the outcome is not known.
    let dials = desk.hooks.opened();
    desk.hooks.sever();
    until("the connection was made again", || {
        desk.hooks.opened() > dials
    });
    desk.current(&world);
    let told = desk
        .routed
        .bots(window(), resume(&copy, unknown))
        .unwrap_err();
    assert_eq!(told.code, bot_linked_codes::BOT_OWNER_UNREACHABLE);
    assert_eq!(delivery(&told), bot_linked_codes::DELIVERY_UNKNOWN);
    assert!(!told.retryable);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(desk.hooks.frames_of(unknown.as_uuid()).len(), 2);
    assert!(
        desk.on_writer(move |state| state.repository.mirror_commands_unconfirmed(owner).unwrap())
            .is_empty()
    );
}

/// `m8_client_commands`: with no stream open nothing is written. A change
/// of a Bot, a message, and a team pause are each refused `not_sent` while
/// the owner cannot be reached, and when its feed is open but no command
/// stream can be opened; no row is recorded and no frame leaves. Nothing
/// is kept to be delivered later: the owner is reached again and nothing
/// arrives.
#[test]
fn m8_client_commands_nothing_is_written_when_no_stream_is_open_team_pause_included() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    desk.link(&world);
    let copy = desk.read(world.bot.id).unwrap();
    let chat = world.bot.main_chat;
    let tries = |desk: &Desk, what: &str| {
        let refusals = [
            desk.routed
                .bots(window(), pause(&copy, BotRequestId::mint()))
                .unwrap_err(),
            desk.routed
                .send_turn(window(), task(&copy, chat, Uuid::new_v4()))
                .err()
                .expect("a message is not sent"),
            desk.routed
                .linked_team_pause(window(), owner, true, BotRequestId::mint(), 0)
                .unwrap_err(),
        ];
        for refusal in refusals {
            assert_eq!(
                refusal.code,
                bot_linked_codes::BOT_OWNER_UNREACHABLE,
                "{what}"
            );
            assert_eq!(
                delivery(&refusal),
                bot_linked_codes::DELIVERY_NOT_SENT,
                "{what}"
            );
            assert!(refusal.retryable, "{what}");
        }
        assert_eq!(desk.rows()[5], 0, "{what}: no command row");
        assert!(desk.hooks.commands.lock().unwrap().is_empty(), "{what}");
    };

    // The owner cannot be reached at all.
    desk.hooks.offline();
    desk.hooks.sever();
    until("the copy is stale", || {
        desk.state(owner) == Some(LinkedOwnerState::Stale)
    });
    tries(&desk, "unreachable");
    // The copy is still read.
    assert_eq!(desk.linked(owner).bots.len(), 1);

    // Its feed is open, and a command stream cannot be opened.
    desk.hooks.refuse_commands.store(true, Ordering::SeqCst);
    desk.hooks.online();
    desk.current(&world);
    tries(&desk, "no command stream");

    // Reached again: nothing that was refused arrives.
    desk.hooks.refuse_commands.store(false, Ordering::SeqCst);
    desk.hooks.sever();
    until("the connection was made again", || {
        desk.state(owner) == Some(LinkedOwnerState::Current)
    });
    std::thread::sleep(Duration::from_millis(400));
    assert!(desk.hooks.commands.lock().unwrap().is_empty());
    assert!(receipts(&world).is_empty());
    assert_eq!(world.begun(), 0);
    let theirs = world.settled_bot();
    assert!(!theirs.paused);
    let team = world.owner;
    assert!(
        !world
            .on_writer(move |state| state.repository.team_pause_view(team).unwrap())
            .paused
    );
}

/// `m8_routing`: one port in front of the local worker. A Bot, a
/// conversation and a message of this desktop reach the local worker
/// unchanged; those of a copy are read from the copy; an id nobody has is
/// the local worker's `not_found`. A change of a copied conversation that
/// its own computer makes is `executing_computer_only`, and a read of what
/// a copy does not hold is `not_found`.
#[test]
fn m8_routing_by_id() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    let local = desk.local.clone().unwrap();
    let linked = world.bot.clone();
    // Before any copy exists the local path is all there is.
    assert_eq!(desk.read(local.id).unwrap().id, local.id);
    assert_eq!(desk.read(linked.id).unwrap_err().code, "not_found");
    desk.link(&world);

    // A Bot.
    assert_eq!(desk.read(local.id).unwrap().owner_device, desk.device);
    assert_eq!(desk.read(linked.id).unwrap().owner_device, owner);
    assert_eq!(desk.read(BotId::mint()).unwrap_err().code, "not_found");
    // The roster of this desktop holds its own Bots only.
    let BotResponse::List { bots } = desk
        .routed
        .bots(
            window(),
            BotCommand::List {
                workspace: local.home_workspace,
            },
        )
        .unwrap()
    else {
        panic!("a list")
    };
    assert_eq!(
        bots.iter().map(|bot| bot.id).collect::<Vec<_>>(),
        [local.id]
    );
    // A conversation.
    let ours = desk.routed.read_chat(window(), local.main_chat).unwrap();
    assert_eq!(ours.chat.id, local.main_chat);
    let theirs = desk.transcript(linked.main_chat);
    assert_eq!(theirs.chat.bot.map(|label| label.bot_id), Some(linked.id));
    assert_eq!(
        desk.routed
            .read_chat(window(), ChatId::new())
            .unwrap_err()
            .code,
        "not_found"
    );
    // A change of a copied conversation only its own computer makes.
    let rename = || koru_ai_wire::RenameChatRequest {
        title: "Renamed".into(),
    };
    assert_eq!(
        desk.routed
            .rename_chat(window(), linked.main_chat, rename())
            .unwrap_err()
            .code,
        bot_linked_codes::EXECUTING_COMPUTER_ONLY
    );
    assert_eq!(
        desk.routed
            .delete_chat(window(), linked.main_chat)
            .unwrap_err()
            .code,
        bot_linked_codes::EXECUTING_COMPUTER_ONLY
    );
    // The same call for this desktop's conversation reaches the local
    // worker, whatever it answers.
    let local_answer = desk.routed.rename_chat(window(), local.main_chat, rename());
    assert!(local_answer.map_or_else(
        |failure| failure.code != bot_linked_codes::EXECUTING_COMPUTER_ONLY,
        |_| true
    ));
    // A read of what a copy does not hold.
    assert_eq!(
        desk.routed
            .usage(window(), Some(linked.main_chat))
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        desk.routed
            .read_chat_tool_policy(window(), linked.main_chat)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        desk.routed
            .bots(window(), BotCommand::ReadLibrary { bot_id: linked.id })
            .unwrap_err()
            .code,
        "not_found"
    );
    // A change of a copied Bot only its own computer makes.
    assert_eq!(
        desk.routed
            .bots(
                window(),
                BotCommand::Trash {
                    request_id: BotRequestId::mint(),
                    bot_id: linked.id,
                    expected_revision: linked.revision,
                },
            )
            .unwrap_err()
            .code,
        bot_linked_codes::EXECUTING_COMPUTER_ONLY
    );
    // A caller that is not a window reads no copy.
    let mut stranger = window();
    stranger.principal.scopes = vec![AiScope::Chat];
    assert_eq!(
        desk.routed
            .bots(stranger, BotCommand::Read { bot_id: linked.id })
            .unwrap_err()
            .code,
        "forbidden"
    );
    // Nothing above was sent anywhere or written to the owner.
    assert!(desk.hooks.commands.lock().unwrap().is_empty());
    assert_eq!(desk.rows()[5], 0);
    assert_eq!(world.read_bot().revision, linked.revision);
}

/// `m8_mirror_states` (not a contract cohort): what a copy is shown as
/// (contract 4.9 and 5.2). An owner that cannot be dialed is "not
/// reachable" and never "needs an update"; an owner that answers with a
/// copy format this build does not read is `unsupported`, nothing of it is
/// applied, its copy stays readable, and no command is sent to it; and the
/// team pause of an owner is read from the copy and changed on the owner.
#[test]
fn m8_mirror_states_unreachable_is_stale_and_an_unknown_format_is_unsupported() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    // Named and not reachable, with nothing ever received: `never`.
    desk.hooks.offline();
    world.port.name(desk.device, &ALL_SCOPES);
    desk.peers.tell(&[owner]);
    until("the owner was dialed", || desk.hooks.opened() >= 2);
    assert_eq!(desk.state(owner), Some(LinkedOwnerState::Never));
    assert!(desk.owners()[0].last_confirmed_at_ms.is_none());
    desk.hooks.online();
    desk.current(&world);

    // The owner's team pause, through the copy: sent to the owner, and
    // read back from the copy when the feed delivered it.
    assert!(desk.owners()[0].team.is_none(), "never paused, never sent");
    let team = desk
        .routed
        .linked_team_pause(window(), owner, true, BotRequestId::mint(), 0)
        .unwrap();
    assert!(team.paused && team.owner_device == owner);
    let copied = desk.owners()[0]
        .team
        .clone()
        .expect("the team pause is in the copy");
    assert_eq!((copied.paused, copied.revision), (true, team.revision));
    // A window with no mirror behind it has no such control.
    let plain = koru_ai_client::Client::new(Arc::new(desk.worker.as_ref().unwrap().client()));
    assert_eq!(
        plain
            .linked_team_pause(window(), owner, false, BotRequestId::mint(), team.revision)
            .unwrap_err()
            .code,
        "unsupported_capability"
    );

    // The connection is lost and the owner cannot be dialed: stale.
    desk.hooks.offline();
    desk.hooks.sever();
    until("the copy is stale", || {
        desk.state(owner) == Some(LinkedOwnerState::Stale)
    });
    let dials = desk.hooks.opened();
    until("the owner was dialed again", || {
        desk.hooks.opened() > dials + 1
    });
    assert_eq!(desk.state(owner), Some(LinkedOwnerState::Stale));

    // The owner answers with a copy format this build does not read.
    desk.hooks.rewrite(|mut message| {
        if let WireMessage::OwnerHello(hello) = &mut message {
            hello.replica_format = 2;
        }
        message
    });
    let before = desk.row(owner);
    desk.hooks.online();
    until("the owner needs an update", || {
        desk.state(owner) == Some(LinkedOwnerState::Unsupported)
    });
    let after = desk.row(owner);
    assert_eq!(
        (after.cursor, after.generation),
        (before.cursor, before.generation)
    );
    // Its copy is still read, and nothing is sent to it.
    let view = desk.linked(owner);
    assert_eq!(
        (view.state, view.bots.len()),
        (LinkedOwnerState::Unsupported, 1)
    );
    let refused = desk
        .routed
        .bots(window(), pause(&view.bots[0], BotRequestId::mint()))
        .unwrap_err();
    assert_eq!(delivery(&refused), bot_linked_codes::DELIVERY_NOT_SENT);
    assert_eq!(desk.rows()[5], 1, "only the team pause was ever recorded");
    // Updated: the next dial is answered in a format this build reads.
    desk.hooks.rewrite(|message| message);
    desk.current(&world);
    assert_eq!(desk.row(owner).generation, before.generation, "no reset");
}

/// A copy with a conversation that has two messages, on a desktop whose
/// mirror deletes one row at a time.
fn copied(runtime: &tokio::runtime::Runtime, world: &World) -> Desk {
    let desk = Desk::new(runtime, world);
    desk.link(world);
    let chat = world.bot.main_chat;
    let _reply = world
        .client()
        .send_turn(local_owner(), task(&world.bot, chat, Uuid::new_v4()))
        .unwrap();
    until("the reply reached the copy", || {
        let messages = desk.transcript(chat).messages;
        messages.len() == 2 && messages.iter().all(|message| message.state.is_terminal())
    });
    assert_eq!(desk.rows()[..4], [1, 1, 1, 2]);
    desk
}

/// `m8_removal_on_revocation`: the owner stops being a device this desktop
/// may keep a copy of. Every read hides the copy at once, while its rows
/// are still on the disk; its streams end and it is not dialed again; and
/// the rows are deleted a batch at a time, the owner row last.
#[test]
fn m8_removal_on_revocation_the_copy_is_hidden_at_once_and_deleted_in_batches() {
    let world = World::new();
    let runtime = runtime();
    let desk = copied(&runtime, &world);
    let owner = world.owner.device;
    let (bot, chat) = (world.bot.id, world.bot.main_chat);

    desk.peers.tell(&[]);
    // At once: the port is asked before the copy is read.
    assert!(desk.owners().is_empty());
    assert_eq!(desk.linked(owner).state, LinkedOwnerState::Never);
    assert!(desk.linked(owner).bots.is_empty());
    assert_eq!(desk.read(bot).unwrap_err().code, "not_found");
    assert_eq!(
        desk.routed.read_chat(window(), chat).unwrap_err().code,
        "not_found"
    );
    // And the rows are still there: four batches come before the owner
    // row, with a pause between them.
    let rows = desk.rows();
    assert_eq!(rows[0], 1, "the owner row goes last");
    assert!(rows[1..4].iter().sum::<i64>() > 0, "{rows:?}");
    until("the copy is being deleted", || {
        desk.on_writer(move |state| state.repository.mirror_owner(owner).unwrap())
            .map_or(true, |row| row.state == LinkedOwnerState::Withdrawing)
    });
    until("the copy was deleted", || desk.rows()[..5] == [0; 5]);
    // Nothing is dialed any more.
    let dials = desk.hooks.opened();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(desk.hooks.opened(), dials);
    assert!(desk.owners().is_empty());
    // This desktop's own Bot is untouched.
    let local = desk.local.clone().unwrap();
    assert_eq!(desk.read(local.id).unwrap().id, local.id);

    // Linked again, the copy starts from nothing and is whole again.
    desk.peers.tell(&[owner]);
    desk.current(&world);
    assert_eq!(desk.rows()[..4], [1, 1, 1, 2]);
    assert_eq!(desk.transcript(chat).messages.len(), 2);
}

/// `m8_removal_on_revocation`: a port that has been told nothing names
/// nobody because nobody was asked. A desktop that starts with a copy and
/// such a port deletes nothing and dials nobody: the copy is read, stale.
/// Once the port is told and does not name the owner, the copy goes.
#[test]
fn m8_removal_on_revocation_a_port_told_nothing_deletes_no_copy_and_dials_nobody() {
    let world = World::new();
    let runtime = runtime();
    let owner = world.owner.device;
    let kept = copied(&runtime, &world).stop();
    // The worker starts again; its admission port has been told nothing.
    let desk = Desk::start(&runtime, &world, kept);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(desk.hooks.opened(), 0, "nobody is dialed");
    assert_eq!(desk.rows()[..4], [1, 1, 1, 2], "nothing is deleted");
    // The copy is read, and says it is not current.
    let view = desk.linked(owner);
    assert_eq!(view.state, LinkedOwnerState::Stale);
    assert!(view.last_confirmed_at_ms.is_some());
    assert_eq!(view.bots.len(), 1);
    assert_eq!(desk.transcript(world.bot.main_chat).messages.len(), 2);
    // No command can be sent: nothing is connected.
    let refused = desk
        .routed
        .bots(window(), pause(&view.bots[0], BotRequestId::mint()))
        .unwrap_err();
    assert_eq!(delivery(&refused), bot_linked_codes::DELIVERY_NOT_SENT);
    assert_eq!(desk.rows()[5], 0);
    // Told, and the owner is not named: now the copy goes.
    desk.peers.tell(&[]);
    until("the copy was deleted", || desk.rows()[..5] == [0; 5]);
    assert_eq!(desk.hooks.opened(), 0);
}

/// `m8_removal_on_revocation`: a delete that a stopped worker left half
/// way is durable. The next start finds the copy still withdrawing, keeps
/// it hidden, and finishes the delete, before the port has said anything.
#[test]
fn m8_removal_on_revocation_a_delete_that_was_interrupted_continues_at_the_next_start() {
    let world = World::new();
    let runtime = runtime();
    let owner = world.owner.device;
    let mut kept = copied(&runtime, &world).stop();
    // An earlier run decided to withdraw the copy, deleted one row, and
    // stopped.
    assert_eq!(
        kept.repository.mirror_withdraw(&[], 1).unwrap(),
        vec![owner]
    );
    assert_eq!(
        kept.repository.mirror_delete_batch(owner, 1).unwrap(),
        koru_ai_sqlite::MirrorDeleted::More
    );
    assert_eq!(
        kept.repository.mirror_row_counts_for_fixture()[..4],
        [1, 1, 1, 1]
    );
    let desk = Desk::start(&runtime, &world, kept);
    // Hidden from the first read on.
    assert!(desk.owners().is_empty());
    assert_eq!(desk.read(world.bot.id).unwrap_err().code, "not_found");
    until("the delete finished", || desk.rows()[..5] == [0; 5]);
    assert_eq!(
        desk.hooks.opened(),
        0,
        "nobody is dialed on a port told nothing"
    );
}

/// `m8_end_to_end_pair`: the owner's worker and a linked desktop's worker
/// over paired streams. The desktop subscribes; sends a message whose
/// reply it follows to its end; pauses the Bot; loses the connection and
/// still reads its copy; is refused an edit while offline, with nothing
/// kept; reconnects without a reset; and a command whose receipt was lost
/// is replayed once, with one outcome on the owner.
#[test]
fn m8_end_to_end_pair() {
    let world = World::new();
    let runtime = runtime();
    let desk = Desk::new(&runtime, &world);
    let owner = world.owner.device;
    let (bot, chat) = (world.bot.id, world.bot.main_chat);

    // Subscribe.
    desk.link(&world);
    assert_eq!(desk.linked(owner).bots.len(), 1);

    // Chat: the message goes to the owner, which runs it; the stream ends
    // with the reply's end, and the copy then holds both messages.
    let reply = desk
        .routed
        .send_turn(window(), task(&world.bot, chat, Uuid::new_v4()))
        .unwrap();
    let mut events: Vec<StreamEvent> = Vec::new();
    while let Ok(event) = reply.events.recv_timeout(Duration::from_secs(30)) {
        let last = event.event.is_terminal();
        events.push(event);
        if last {
            break;
        }
    }
    // What the owner forwarded while the reply was written comes first,
    // in order and of this reply. How much of it arrives is not asserted:
    // the protocol does not acknowledge a watch, so a reply may begin
    // before the owner registered it. The end always arrives.
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert!(
        events
            .iter()
            .all(|event| event.event.attempt_id() == reply.accepted.attempt_id)
    );
    let end = events.last().expect("the stream carried the reply's end");
    assert!(end.event.is_terminal(), "{end:?}");
    assert_eq!(end.event.attempt_id(), reply.accepted.attempt_id);
    assert_eq!(world.begun(), 1, "the owner ran the task once");
    let transcript = desk.transcript(chat);
    assert_eq!(
        transcript
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [
            reply.accepted.user_message_id,
            reply.accepted.assistant_message_id
        ]
    );
    assert_eq!(transcript.messages[1].state, MessageState::Complete);
    // What the copy shows is what the owner's own window reads.
    let theirs = world.client().read_chat(local_owner(), chat).unwrap();
    assert_eq!(transcript.messages, theirs.messages);
    assert_eq!(transcript.blocks, theirs.blocks);
    assert!(!transcript.messages[1].markdown.is_empty());
    // A message of the copy takes no feedback here.
    assert_eq!(
        desk.routed
            .feedback(
                window(),
                reply.accepted.assistant_message_id,
                koru_ai_wire::FeedbackRequest {
                    event_id: koru_ai_core::FeedbackEventId::new(),
                    value: koru_ai_core::FeedbackValue::Good,
                    surface: AiSurface::Standalone,
                },
            )
            .unwrap_err()
            .code,
        bot_linked_codes::EXECUTING_COMPUTER_ONLY
    );

    // Pause: answered when the copy shows it.
    let copy = desk.read(bot).unwrap();
    desk.routed
        .bots(window(), pause(&copy, BotRequestId::mint()))
        .unwrap();
    assert!(desk.read(bot).unwrap().paused);
    assert!(world.settled_bot().paused);

    // Disconnect, and read the copy.
    desk.hooks.offline();
    desk.hooks.sever();
    until("the copy is stale", || {
        desk.state(owner) == Some(LinkedOwnerState::Stale)
    });
    let view = desk.linked(owner);
    assert!(view.last_confirmed_at_ms.is_some());
    assert!(view.bots[0].paused);
    assert_eq!(desk.transcript(chat).messages, transcript.messages);

    // An edit while offline is refused, and nothing is kept for later.
    let copy = desk.read(bot).unwrap();
    let mut config = copy.config.clone();
    config.name = "Renamed while offline".into();
    let commands = desk.rows()[5];
    let refused = desk
        .routed
        .bots(
            window(),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot,
                expected_revision: copy.revision,
                config,
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, bot_linked_codes::BOT_OWNER_UNREACHABLE);
    assert_eq!(delivery(&refused), bot_linked_codes::DELIVERY_NOT_SENT);
    assert_eq!(desk.rows()[5], commands);
    assert_eq!(desk.read(bot).unwrap().config.name, copy.config.name);

    // Reconnect: the copy continues from its cursor, with no reset.
    let generation = desk.row(owner).generation;
    desk.hooks.online();
    desk.current(&world);
    assert_eq!(desk.row(owner).generation, generation);
    assert_eq!(world.read_bot().config.name, copy.config.name);

    // One replay and one outcome: the receipt of a resume is lost, the
    // connection is made again, and the mirror asks with the same bytes.
    let copy = desk.read(bot).unwrap();
    let lost = BotRequestId::mint();
    desk.hooks.cut_command.store(true, Ordering::SeqCst);
    let failure = desk.routed.bots(window(), resume(&copy, lost)).unwrap_err();
    assert_eq!(delivery(&failure), bot_linked_codes::DELIVERY_NOT_CONFIRMED);
    until("the owner applied the resume", || {
        !world.settled_bot().paused
    });
    desk.hooks.sever();
    until("the resume was confirmed", || {
        desk.command(owner, lost.as_uuid())
            .is_some_and(|row| row.state == MirrorCommandState::Receipted)
    });
    let row = desk.command(owner, lost.as_uuid()).unwrap();
    assert!(matches!(
        row.outcome,
        Some(ClientOutcome::Completed | ClientOutcome::Accepted)
    ));
    let frames = desk.hooks.frames_of(lost.as_uuid());
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], frames[1]);
    assert_eq!(
        receipts(&world)
            .iter()
            .filter(|(kind, state, _)| kind == "resume_bot" && state == "done")
            .count(),
        1
    );
    assert_eq!(
        world
            .local
            .applied()
            .iter()
            .filter(|kind| **kind == "resume_bot")
            .count(),
        1
    );
    until("the copy shows the resume", || {
        desk.read(bot).is_ok_and(|bot| !bot.paused)
    });
    assert_eq!(world.begun(), 1, "nothing ran twice");
    audit_refused_nothing(&world);
}

// Cohort `m8_events_content_free`: the two rows a linked desktop publishes.
mod events;
