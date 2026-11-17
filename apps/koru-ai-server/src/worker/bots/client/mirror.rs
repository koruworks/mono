// SPDX-License-Identifier: MPL-2.0

//! The client mirror (M8, P8.3 piece I): the copy this computer keeps of
//! the Bots another of the person's computers runs, and the way to that
//! computer.
//!
//! "Owner" is the computer that runs the Bots. This computer is its client.
//! For each owner the admission port names, one task ([`follow`]) keeps a
//! feed stream open: it says hello, subscribes from the cursor the copy
//! holds, applies each page in one transaction on the AI writer, and
//! acknowledges it. A command the person gives here for one of those Bots
//! is written to a command stream of that owner ([`send`]) and is answered
//! by that owner's receipt. Reads are answered from the copy ([`copy`]).
//!
//! # What a copy can and cannot do
//!
//! - **A copy is read-only and nothing runs from it.** The only writer of
//!   the copy tables is the page applier. No code path leads from a copied
//!   row to a submission, an attempt, a session, a Run, a Routine or a
//!   tool: this module holds no handle to any of them, and its work on the
//!   writer reads and writes the six client tables and nothing else.
//! - **A command is sent only on a stream that is open.** With the owner
//!   unreachable the caller is told so and nothing is recorded: nothing
//!   waits to be delivered later, a team pause included.
//! - **A command is never sent twice by a person.** Its row is written
//!   before its bytes are, and a command whose answer did not arrive is
//!   sent again with the same bytes when the owner is next reached, where
//!   the owner's durable receipt answers it without applying it again.
//! - **An owner cannot claim a Bot of this computer or of another owner**:
//!   a page that names one is refused whole and its stream is closed.
//!
//! # Who is an owner
//!
//! The admission port ([`OwnerPeers`]). An answer with sequence 0 means the
//! port has been told nothing yet: no copy is deleted and nobody is dialed
//! on it. An owner the port stops naming is hidden from every read at once
//! and its copy is deleted in bounded batches; a delete that was
//! interrupted continues when this starts again.
//!
//! An own device may be a phone, which runs no Bots and closes every Bot
//! dial. A dial that fails for any reason is "not reachable" and is tried
//! again with a growing pause; "needs an update" is said only when an owner
//! answered that the two share no protocol version or copy format.
//!
//! # Where it runs
//!
//! Every stream is a task on the runtime the mirror was started in. `ai.db`
//! is reached through short calls on the AI writer; nothing here waits on a
//! stream, on the Store or on a dial while it is on the writer, and no
//! window's request waits behind a page.
//!
//! Nothing a person or a Bot wrote is logged from here: a page is logged by
//! its cursors, a command by its kind and outcome, a refusal by its reason.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_ai_core::bots::{BotAuthority, BotError, DeviceId};
use koru_ai_core::{ChatId, InferenceAttemptId, Timestamp, ToolInvocationId};
use koru_ai_wire::{Failure, bot_linked_codes};
use koru_bot_client_wire::{FrameDecoder, Negotiated, WireMessage, encode, limits};
use koru_network_edge::{NetworkEgress, TransportSession};

use super::owner::{Gone, WriterCall};
use crate::worker::{Envelope, WorkerState};

pub(crate) mod copy;
mod follow;
mod send;

#[cfg(test)]
mod tests;

/// Which devices this computer may keep a copy of the Bots of: the
/// admission port, as the mirror reads it.
pub(crate) trait OwnerPeers: Send + Sync + 'static {
    fn owners(&self) -> OwnersNow;
    /// A wake-up when the answer changes.
    fn changed(&self) -> tokio::sync::watch::Receiver<u64>;
}

/// The admission port's answer at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnersNow {
    /// 0 until the port was told anything. **An answer with sequence 0
    /// names nobody because nobody was asked**, not because nobody is an
    /// owner: nothing is deleted and nobody is dialed on it.
    pub(crate) sequence: u64,
    pub(crate) owners: BTreeSet<DeviceId>,
}

impl OwnerPeers for koru_store_server::BotPeers {
    fn owners(&self) -> OwnersNow {
        let current = self.current();
        OwnersNow {
            sequence: current.sequence,
            owners: current
                .peers
                .iter()
                .filter(|(_, peer)| peer.owner)
                .map(|(device, _)| *device)
                .collect(),
        }
    }

    fn changed(&self) -> tokio::sync::watch::Receiver<u64> {
        koru_store_server::BotPeers::changed(self)
    }
}

/// How long the mirror waits, and how much it does at once.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// The pause before the first redial of an owner that was not reached.
    pub(crate) backoff_first: Duration,
    /// The longest pause between two dials of one owner.
    pub(crate) backoff_max: Duration,
    /// The pause before an owner that needs an update is asked again.
    pub(crate) unsupported_retry: Duration,
    /// How long a dial and the owner's answer to a hello may take.
    pub(crate) hello: Duration,
    /// A feed that says nothing for this long is treated as closed.
    pub(crate) silence: Duration,
    /// How long a command waits for its receipt before it is "not
    /// confirmed".
    pub(crate) receipt: Duration,
    /// How long an answered command waits for the copy to show its effect.
    pub(crate) settle: Duration,
    /// How long a caller of this computer waits for a linked answer.
    pub(crate) call: Duration,
    /// How often the commands this computer sent are swept, and how soon a
    /// stopping worker is noticed.
    pub(crate) sweep: Duration,
    /// How many rows of a withdrawn copy one writer call deletes.
    pub(crate) delete_batch: u32,
    /// The pause between two such calls, so the writer serves others.
    pub(crate) delete_pause: Duration,
    /// The pause before a command the owner is still applying is asked
    /// about again.
    pub(crate) resend_pause: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            backoff_first: Duration::from_secs(2),
            backoff_max: Duration::from_secs(600),
            unsupported_retry: Duration::from_secs(3600),
            hello: Duration::from_millis(limits::HELLO_TIMEOUT_MS),
            silence: Duration::from_millis(limits::FEED_SILENCE_TIMEOUT_MS),
            // The owner's own local command may take 30 seconds.
            receipt: Duration::from_secs(45),
            settle: Duration::from_secs(10),
            // A dial and a hello, the receipt, and the copy catching up.
            call: Duration::from_secs(80),
            sweep: Duration::from_secs(60),
            delete_batch: 500,
            delete_pause: Duration::from_millis(20),
            resend_pause: Duration::from_secs(3),
        }
    }
}

/// What the worker hands over for a mirror to be built from: its inbox, its
/// stop flag and its Bot authority, which names this device.
#[derive(Clone)]
pub(crate) struct Parts {
    pub(in crate::worker) sender: SyncSender<Envelope>,
    pub(in crate::worker) shutdown: Arc<AtomicBool>,
    pub(crate) authority: Arc<dyn BotAuthority>,
}

/// The client mirror. Cheap to clone; every clone is the same mirror.
#[derive(Clone)]
pub struct BotMirror {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BotMirror {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("BotMirror").finish_non_exhaustive()
    }
}

impl BotMirror {
    /// Start keeping copies of the Bots of the owners `peers` names, dialed
    /// through `egress`. The mirror runs on `runtime` from here on and
    /// stops with the worker.
    ///
    /// **This is the only way a Bot stream is ever opened from this
    /// computer and the only way a copy table is ever written.** A worker
    /// for which it is not called dials nobody and writes nothing.
    pub(crate) fn start(
        parts: Parts,
        egress: Arc<dyn NetworkEgress>,
        peers: Arc<dyn OwnerPeers>,
        timing: Timing,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let shared = Arc::new(Shared {
            link: Link {
                sender: parts.sender,
                shutdown: parts.shutdown,
            },
            authority: parts.authority,
            egress,
            peers,
            timing,
            runtime: runtime.clone(),
            this_device: tokio::sync::OnceCell::new(),
            owners: Mutex::default(),
            deleting: Mutex::default(),
            index: Mutex::default(),
            copies: AtomicUsize::new(0),
            ready: tokio::sync::watch::Sender::new(false),
            kick: tokio::sync::Notify::new(),
        });
        runtime.spawn(Arc::clone(&shared).supervise());
        Self { shared }
    }

    /// The linked computers' Bots, as the routing port reads them.
    pub fn linked(&self) -> Arc<dyn koru_ai_client::LinkedBots> {
        Arc::new(self.clone())
    }
}

/// The way to the AI writer. A call is one short piece of work on the
/// writer thread; the caller waits for its answer without holding a thread.
#[derive(Clone)]
struct Link {
    sender: SyncSender<Envelope>,
    shutdown: Arc<AtomicBool>,
}

impl Link {
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
            if self.stopping() {
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

    /// A repository call on the writer, with a failure of either kind as
    /// `None`.
    async fn stored<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut WorkerState) -> Result<T, BotError> + Send + 'static,
    ) -> Option<T> {
        self.on_writer(work).await.ok()?.ok()
    }
}

/// One owner the mirror follows.
struct Owner {
    device: DeviceId,
    /// A feed stream to this owner is open and its hello was answered.
    connected: AtomicBool,
    /// What that stream agreed on. A control whose capability is absent is
    /// not offered.
    negotiated: Mutex<Option<Negotiated>>,
    /// The cursor of the visible copy, published after each page.
    cursor: tokio::sync::watch::Sender<u64>,
    /// One command to this owner at a time.
    lane: tokio::sync::Mutex<()>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Owner {
    fn new(device: DeviceId, cursor: u64) -> Self {
        Self {
            device,
            connected: AtomicBool::new(false),
            negotiated: Mutex::default(),
            cursor: tokio::sync::watch::Sender::new(cursor),
            lane: tokio::sync::Mutex::new(()),
            task: Mutex::default(),
        }
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn has(&self, capability: &str) -> bool {
        lock(&self.negotiated)
            .as_ref()
            .is_some_and(|negotiated| negotiated.has(capability))
    }

    fn stop(&self) {
        self.connected.store(false, Ordering::Release);
        if let Some(task) = lock(&self.task).take() {
            task.abort();
        }
    }
}

/// The replies and pending calls of the copies that a command may name,
/// with the owner and conversation each belongs to. A copy does not index
/// them; they are learned from the records as pages are applied, from the
/// owner's answers, and once at start from the conversations that were
/// running.
#[derive(Default)]
struct Index {
    attempts: HashMap<InferenceAttemptId, (DeviceId, ChatId)>,
    tools: HashMap<ToolInvocationId, (DeviceId, ChatId)>,
}

/// More than this many entries of one kind and the index starts again; it
/// holds what is running now, not a history.
const INDEX_LIMIT: usize = 8192;
/// How many of a running conversation's newest messages are read at start
/// to find its reply and its pending calls again.
const ACTIVE_MESSAGES: u32 = 4;

impl Index {
    fn learn(&mut self, owner: DeviceId, learned: copy::Learned) {
        if self.attempts.len() > INDEX_LIMIT {
            self.attempts.clear();
        }
        if self.tools.len() > INDEX_LIMIT {
            self.tools.clear();
        }
        for (attempt, chat) in learned.attempts {
            self.attempts.insert(attempt, (owner, chat));
        }
        for (tool, chat) in learned.tools {
            self.tools.insert(tool, (owner, chat));
        }
    }

    fn forget(&mut self, keep: impl Fn(&DeviceId) -> bool) {
        self.attempts.retain(|_, (owner, _)| keep(owner));
        self.tools.retain(|_, (owner, _)| keep(owner));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn now_ms() -> i64 {
    Timestamp::now().millis()
}

struct Shared {
    link: Link,
    authority: Arc<dyn BotAuthority>,
    egress: Arc<dyn NetworkEgress>,
    peers: Arc<dyn OwnerPeers>,
    timing: Timing,
    runtime: tokio::runtime::Handle,
    /// This device, as the Bot authority names it: the hello's
    /// `client_device`. Asked once, on a blocking thread.
    this_device: tokio::sync::OnceCell<DeviceId>,
    owners: Mutex<HashMap<DeviceId, Arc<Owner>>>,
    /// The owners whose copy is being deleted by a task of this run.
    deleting: Mutex<HashSet<DeviceId>>,
    index: Mutex<Index>,
    /// How many owner rows there are. With none, a lookup answers without
    /// reading anything.
    copies: AtomicUsize,
    /// True once this run has looked at what the last one left.
    ready: tokio::sync::watch::Sender<bool>,
    /// Look at the admission port again.
    kick: tokio::sync::Notify,
}

impl Shared {
    fn owner(&self, device: DeviceId) -> Option<Arc<Owner>> {
        lock(&self.owners).get(&device).cloned()
    }

    /// Whether a copy of `device` may be read right now: the admission
    /// port is asked first, so an owner it stopped naming is hidden before
    /// the writer has marked its copy. A port that was told nothing hides
    /// nothing.
    fn visible(&self, device: &DeviceId) -> bool {
        let now = self.peers.owners();
        now.sequence == 0 || now.owners.contains(device)
    }

    async fn count_copies(&self) {
        if let Some(count) = self
            .link
            .stored(|state| state.repository.mirror_owner_count())
            .await
        {
            self.copies.store(
                usize::try_from(count).unwrap_or(usize::MAX),
                Ordering::Release,
            );
        }
    }

    /// The mirror's own loop: what the last run left, then the admission
    /// port for as long as the worker runs.
    async fn supervise(self: Arc<Self>) {
        let now = now_ms();
        let started = self
            .link
            .stored(move |state| {
                state.repository.mirror_recover(now)?;
                state.repository.mirror_commands_sweep(now)?;
                Ok((
                    state.repository.mirror_withdrawing()?,
                    state.repository.mirror_owner_count()?,
                    state.repository.mirror_active_messages(ACTIVE_MESSAGES)?,
                ))
            })
            .await;
        let Some((withdrawing, copies, active)) = started else {
            tracing::warn!("the copies of linked computers' Bots could not be read; none is kept");
            // Nothing is copied by this run: a lookup answers at once.
            self.ready.send_replace(true);
            return;
        };
        self.copies.store(
            usize::try_from(copies).unwrap_or(usize::MAX),
            Ordering::Release,
        );
        {
            let mut index = lock(&self.index);
            for (owner, record) in active {
                index.learn(owner, copy::learn_stored(&record));
            }
        }
        // A delete an earlier run began continues whatever the port says
        // now: it was decided when the port named somebody else.
        for owner in withdrawing {
            self.delete(owner);
        }
        self.ready.send_replace(true);
        let mut changed = self.peers.changed();
        let mut port_open = true;
        loop {
            if self.link.stopping() {
                break;
            }
            if self.reconcile().await.is_err() {
                break;
            }
            tokio::select! {
                result = changed.changed(), if port_open => {
                    if result.is_err() {
                        port_open = false;
                    }
                }
                () = self.kick.notified() => {}
                () = tokio::time::sleep(self.timing.sweep) => {
                    let now = now_ms();
                    let _ = self
                        .link
                        .stored(move |state| state.repository.mirror_commands_sweep(now))
                        .await;
                }
            }
        }
        for (_, owner) in lock(&self.owners).drain() {
            owner.stop();
        }
    }

    /// Bring the tasks and the copies in line with the admission port.
    async fn reconcile(self: &Arc<Self>) -> Result<(), Gone> {
        let port = self.peers.owners();
        // Not told: nobody is dialed and nothing is deleted.
        if port.sequence == 0 {
            return Ok(());
        }
        let named: Vec<DeviceId> = port.owners.iter().copied().collect();
        let now = now_ms();
        let asked = named.clone();
        let Ok(withdrawing) = self
            .link
            .on_writer(move |state| state.repository.mirror_withdraw(&asked, now))
            .await?
        else {
            return Ok(());
        };
        // The streams of an owner the port stopped naming end here.
        lock(&self.owners).retain(|device, owner| {
            let keep = port.owners.contains(device);
            if !keep {
                owner.stop();
            }
            keep
        });
        lock(&self.index).forget(|owner| port.owners.contains(owner));
        for owner in &withdrawing {
            self.delete(*owner);
        }
        for device in named {
            // A copy that is still being deleted is deleted first; its
            // owner is followed again when the delete has finished.
            if withdrawing.contains(&device) || lock(&self.owners).contains_key(&device) {
                continue;
            }
            let Ok(row) = self
                .link
                .on_writer(move |state| state.repository.mirror_ensure_owner(device, now))
                .await?
            else {
                continue;
            };
            self.count_copies().await;
            let owner = Arc::new(Owner::new(device, row.cursor));
            let task = self
                .runtime
                .spawn(Arc::clone(self).follow(Arc::clone(&owner)));
            *lock(&owner.task) = Some(task);
            lock(&self.owners).insert(device, owner);
        }
        Ok(())
    }

    /// Delete a withdrawn copy in bounded batches, one short writer call
    /// each, with a pause between them.
    fn delete(self: &Arc<Self>, owner: DeviceId) {
        if !lock(&self.deleting).insert(owner) {
            return;
        }
        let shared = Arc::clone(self);
        self.runtime.spawn(async move {
            let batch = shared.timing.delete_batch;
            loop {
                let step = shared
                    .link
                    .stored(move |state| state.repository.mirror_delete_batch(owner, batch))
                    .await;
                match step {
                    Some(koru_ai_sqlite::MirrorDeleted::More) => {
                        tokio::time::sleep(shared.timing.delete_pause).await;
                    }
                    Some(koru_ai_sqlite::MirrorDeleted::Done) => {
                        tracing::info!("a linked computer's copy of Bots was deleted");
                        break;
                    }
                    // The worker is stopping, or storage failed: the row
                    // stays `withdrawing` and the next start continues.
                    None => break,
                }
            }
            lock(&shared.deleting).remove(&owner);
            shared.count_copies().await;
            shared.kick.notify_one();
        });
    }

    /// This device's id, as the hello names it.
    async fn this_device(&self) -> Option<DeviceId> {
        self.this_device
            .get_or_try_init(|| async {
                let authority = Arc::clone(&self.authority);
                tokio::task::spawn_blocking(move || authority.owner())
                    .await
                    .map_err(|_| ())?
                    .map(|owner| owner.device)
                    .map_err(|_| ())
            })
            .await
            .ok()
            .copied()
    }
}

/// What a worker that keeps no copy answers the two linked reads: no
/// linked Bots. `None` for any other command. A worker whose mirror runs
/// never gets them: the routing port answers them from the copy.
pub(crate) fn without_copy(
    context: &koru_ai_api::RequestContext,
    command: &koru_ai_core::bots::BotCommand,
) -> Option<Result<koru_ai_core::bots::BotResponse, Failure>> {
    use koru_ai_core::bots::{BotCommand, BotResponse, LinkedBotsView, LinkedOwnerState};
    let answer = match command {
        BotCommand::ListLinkedOwners {} => BotResponse::LinkedOwners { owners: Vec::new() },
        BotCommand::ListLinkedBots { owner_device, .. } => BotResponse::LinkedBots {
            linked: LinkedBotsView {
                owner_device: *owner_device,
                state: LinkedOwnerState::Never,
                last_confirmed_at_ms: None,
                bots: Vec::new(),
            },
        },
        _ => return None,
    };
    Some(crate::worker::authorize(context, koru_ai_api::AiScope::ManageBots).map(|()| answer))
}

/// What reading a stream gave.
enum Read {
    Message(WireMessage),
    /// The owner closed the stream, or the carrier ended it.
    Closed,
    /// Nothing arrived in the time allowed.
    Silent,
    /// A malformed or oversized frame, or a message of an unknown type.
    Malformed,
}

/// This computer's half of one stream to an owner.
struct Wire {
    session: TransportSession,
    decoder: FrameDecoder,
}

impl Wire {
    fn new(session: TransportSession) -> Self {
        Self {
            session,
            decoder: FrameDecoder::new(),
        }
    }

    /// The next whole message, or why there is none within `within`.
    async fn read(&mut self, within: Duration) -> Read {
        let next = async {
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
        };
        tokio::time::timeout(within, next)
            .await
            .unwrap_or(Read::Silent)
    }

    async fn send(&self, message: WireMessage, within: Duration) -> Result<(), ()> {
        let frame = encode(&message).map_err(|_| ())?;
        self.send_frame(frame, within).await
    }

    /// Write the body of one frame exactly as given.
    async fn send_body(&self, body: &[u8], within: Duration) -> Result<(), ()> {
        let length = u32::try_from(body.len()).map_err(|_| ())?;
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(body);
        self.send_frame(frame, within).await
    }

    async fn send_frame(&self, frame: Vec<u8>, within: Duration) -> Result<(), ()> {
        match tokio::time::timeout(within, self.session.send.send(frame)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(()),
        }
    }
}

/// The owner cannot be reached, or the outcome of what was sent is not
/// known. `delivery` says which (`koru_ai_wire::bot_linked_codes`).
fn unreachable(delivery: &str) -> Failure {
    let mut failure = crate::worker::failure(
        bot_linked_codes::BOT_OWNER_UNREACHABLE,
        match delivery {
            bot_linked_codes::DELIVERY_NOT_SENT => {
                "The computer that runs the Bot cannot be reached. Nothing was sent."
            }
            bot_linked_codes::DELIVERY_NOT_CONFIRMED => {
                "The computer that runs the Bot did not confirm this yet."
            }
            _ => "The computer that runs the Bot cannot say whether this took effect.",
        },
        delivery == bot_linked_codes::DELIVERY_NOT_SENT,
    );
    failure.details.insert(
        "delivery".into(),
        serde_json::Value::String(delivery.into()),
    );
    failure
}

fn unsupported() -> Failure {
    crate::worker::failure(
        bot_linked_codes::BOT_OWNER_UNSUPPORTED,
        "The computer that runs the Bot and this one need the same version of Koru for this.",
        false,
    )
}
