// SPDX-License-Identifier: MPL-2.0

//! Talking to the Koru Store: one document's worth of sync.
//!
//! [`Session`](crate::session::Session) does `export → transport → import` over
//! a folder — a snapshot, and an append-only log per peer. This is the same
//! cycle over the store: `export → send_update → next_event → import`. The
//! shape is deliberately identical, because the point of the swap is that
//! everything above it keeps working.
//!
//! **What is different, and it is the reason the store exists**: an
//! acknowledgement means the bytes are on the disk
//! ([ADR-79](../../../docs/decisions/ADR-076-100.md#adr-79-durable--an-acknowledgement-means-the-bytes-are-on-the-disk)).
//! The folder transport appends and hopes; here a write is answered, and
//! [`Ack::durable`] is the answer. Nothing above needs to know that yet, but it
//! is what makes "did my edit survive" answerable at all.
//!
//! ## Threaded, and it had to be
//!
//! `Connection::next_event` blocks, and this used to call it on the event
//! loop's thread with a 1 ms read timeout — once per open session, every turn.
//! It was polled rather than threaded on the reasoning that the poll was short
//! enough not to matter, and the reasoning was wrong in a way only a
//! measurement would show. From a running window, idle, nobody typing:
//!
//! ```text
//! sync x151 /s  publish 0.8ms  drain 365.7ms (read 365.5ms)  | sessions 2
//! ```
//!
//! **365 ms of every second — 37% of the event loop — blocked in a socket
//! read.** Not because a read was slow, but because [`Workspace::next_due`]
//! reported *now* whenever a store session existed, so the loop spun as fast as
//! it could and paid a read per session per spin. Shortening the timeout made
//! the loop spin faster, which is why dropping it from 20 ms to 1 ms was felt as
//! almost nothing.
//!
//! The socket is on a thread now (`link::thread`), and this module's share of a
//! turn is a channel `try_recv`. The numbers are in
//! [RUNBOOK](../../../docs/contributing/operations/RUNBOOK.md) and reproducible with
//! `cargo test -p koru-store-server --test perf a_quiet_sync_turn`.
//!
//! [`Workspace::next_due`]: crate::Workspace::next_due

use std::collections::HashSet;
use std::sync::Arc;

use koru_api_client::{Ack, Client, Event};
use koru_core::schema::WorkspaceDoc;
use koru_store_core::{
    errors::Error,
    ids::{DocumentId as StoreDocumentId, WorkspaceId as StoreWorkspaceId},
};
use loro_protocol::BatchId;

pub mod link;
pub mod outbox;
pub mod revisions;
use link::{Inbound, Link, ThreadLink};

/// Boundary-owned client construction used by explicit embedded/test hosts.
pub type ClientFactory = Arc<dyn Fn() -> Result<Client, Error> + Send + Sync>;

/// A room id as it appears on the wire.
///
/// **Built here rather than imported.** The grammar lives in
/// `koru_store_api::rooms`, which is the *server*; depending on it to format a
/// string would drag axum and tokio into every client. Two small functions and
/// a test that pins the spelling is the cheaper half of that trade — and the
/// spelling is wire format, so it could not be changed unilaterally on either
/// side anyway.
pub fn document_room(id: StoreDocumentId) -> String {
    format!("document:{}", id.to_storage())
}

/// The room carrying a workspace's list of documents.
pub fn manifest_room(id: StoreWorkspaceId) -> String {
    format!("workspace:{}:manifest", id.to_storage())
}

/// What the store can sync: anything that exports Loro updates and imports them.
///
/// Both things a workspace is made of satisfy this with the *same method names
/// and the same signatures* — `WorkspaceDoc` for a document's content, and the
/// store's `Manifest` for the list of documents. That is not a coincidence
/// worth exploiting quietly: they are both a `LoroDoc` behind a façade, and the
/// four calls below are the whole of what a transport needs from one.
///
/// A trait rather than two near-identical sessions, because the interesting
/// parts — the version-vector guard, the bounded drain, the durability
/// bookkeeping — are the parts that would be copied, and a copy is where the
/// two would drift.
pub trait Synced {
    fn version(&self) -> loro::VersionVector;
    fn export_all_updates(&self) -> Result<Vec<u8>, String>;
    fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>, String>;
    fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<(), String>;
    /// This replica's Loro peer id — the name its outbox file takes.
    ///
    /// **Here because it names a replica, which is exactly what the outbox has
    /// to keep apart.** Two windows on one document are two peer ids, so they
    /// are two files and nothing has to be locked.
    fn peer_id(&self) -> u64;
}

impl Synced for WorkspaceDoc {
    fn version(&self) -> loro::VersionVector {
        WorkspaceDoc::version(self)
    }
    fn peer_id(&self) -> u64 {
        WorkspaceDoc::peer_id(self)
    }
    fn export_all_updates(&self) -> Result<Vec<u8>, String> {
        WorkspaceDoc::export_all_updates(self).map_err(|e| e.to_string())
    }
    fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>, String> {
        WorkspaceDoc::export_updates_since(self, from).map_err(|e| e.to_string())
    }
    fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<(), String> {
        WorkspaceDoc::import_frames(self, frames).map_err(|e| e.to_string())
    }
}

impl Synced for koru_core::system::SystemDoc {
    fn version(&self) -> loro::VersionVector {
        koru_core::system::SystemDoc::version(self)
    }
    fn peer_id(&self) -> u64 {
        koru_core::system::SystemDoc::peer_id(self)
    }
    fn export_all_updates(&self) -> Result<Vec<u8>, String> {
        koru_core::system::SystemDoc::export_all_updates(self).map_err(|e| e.to_string())
    }
    fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>, String> {
        koru_core::system::SystemDoc::export_updates_since(self, from).map_err(|e| e.to_string())
    }
    fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<(), String> {
        koru_core::system::SystemDoc::import_frames(self, frames).map_err(|e| e.to_string())
    }
}

impl Synced for koru_store_core::manifest::Manifest {
    fn version(&self) -> loro::VersionVector {
        koru_store_core::manifest::Manifest::version(self)
    }
    fn peer_id(&self) -> u64 {
        koru_store_core::manifest::Manifest::peer_id(self)
    }
    fn export_all_updates(&self) -> Result<Vec<u8>, String> {
        koru_store_core::manifest::Manifest::export_all_updates(self).map_err(|e| e.to_string())
    }
    fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>, String> {
        koru_store_core::manifest::Manifest::export_updates_since(self, from)
            .map_err(|e| e.to_string())
    }
    fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<(), String> {
        koru_store_core::manifest::Manifest::import_frames(self, frames).map_err(|e| e.to_string())
    }
}

/// One document, synced with the store.
///
/// **The bookkeeping is here and the socket is not.** What this replica has
/// handed over, what the store has confirmed, and what is therefore still owed
/// are claims about a *document*, and the document never leaves the thread that
/// edits it. The [`Link`] below moves bytes and knows none of that.
pub struct StoreSession {
    client: Client,
    link: Arc<dyn Link>,
    workspace: StoreWorkspaceId,
    document: StoreDocumentId,
    room: String,
    /// What has been handed to the store. `None` before the first publish, when
    /// everything is owed.
    sent: Option<loro::VersionVector>,
    /// What the store has **confirmed is on its disk**, and therefore the point
    /// [`sent`](Self::sent) rewinds to when anything goes wrong.
    ///
    /// The distinction between this and `sent` is the whole of the durability
    /// bookkeeping. `sent` is a claim about what left this process; `acked` is a
    /// claim about what survived. Only the second is worth anything after a
    /// crash, and until it existed there was nothing to rewind *to* — a refused
    /// acknowledgement dropped those operations from the outbound stream and
    /// nothing ever sent them again.
    acked: Option<loro::VersionVector>,
    /// Batches the store has called durable.
    ///
    /// Kept as a set rather than a count because the question worth answering
    /// is "did *this* edit land", and because an ack can arrive out of order
    /// with respect to the sends.
    durable: HashSet<BatchId>,
    /// Batches sent and not yet answered, **in send order**, each carrying what
    /// `sent` became when it went out.
    ///
    /// A `HashSet<BatchId>` before, which could answer "how many are owed" and
    /// nothing else — so nothing consumed it, and an update in flight when the
    /// connection died was simply lost. The version is what makes an
    /// acknowledgement mean something: it is the point `acked` advances to.
    inflight: std::collections::VecDeque<(BatchId, loro::VersionVector)>,
    /// The room needs joining again before the next publish. Set by a
    /// `RejoinSuggested`, which is the one room error that means *recover*.
    rejoin: bool,
    /// This session has given up, and why. Reached by a refused join, a room
    /// error that is not `RejoinSuggested`, or several refused updates in a row.
    stopped: Option<String>,
    /// Consecutive refused acknowledgements. Reset by any that lands.
    refusals: u32,
    /// Where this session keeps what the store has not confirmed.
    outbox: Buffering,
    /// The version the file on disk covers, so an unchanged document is not
    /// re-exported and rewritten several times a second.
    buffered: Option<loro::VersionVector>,
    /// The file itself, so it can be removed when the store confirms what it
    /// held. Kept rather than recomputed: the path is derived from the peer id,
    /// and a session that could not resolve a root has no path at all.
    buffer_path: Option<std::path::PathBuf>,
    /// The last attempt, whether or not it wrote — the throttle's clock.
    buffered_at: Option<std::time::Instant>,
    /// Why the last attempt did not write, if it did not.
    ///
    /// **The difference between "saved on this machine" and "saved nowhere"**,
    /// which is the difference between a closing window that is safe and one
    /// that is not. Nothing else can tell them apart.
    buffer_failure: Option<String>,
    /// Files taken back off the disk on open and **not yet deleted**.
    ///
    /// Replaying puts the work in memory, which is where it already was when it
    /// was buffered. Deleting the file at that moment would mean a process that
    /// died before the store confirmed anything had thrown the copy away and
    /// kept nothing — the precise failure this whole module exists to prevent.
    replayed: Vec<std::path::PathBuf>,
    /// The document version that covers everything [`replayed`](Self::replayed)
    /// held, so an acknowledgement can be compared against it.
    replayed_covered: Option<loro::VersionVector>,
    /// Whether the replay said anything out loud, so the confirmation that
    /// follows it only speaks if there was something to confirm.
    replay_reported: bool,
}

/// Where a session buffers, if it buffers at all.
enum Buffering {
    /// No disk behind this session — the fake-link tests, and anything
    /// in-memory. Nothing is written, and nothing is claimed about safety.
    Off,
    /// The store root to buffer under. `None` means this build's channel root;
    /// `Some` is the `--root` the store was found at, which is what keeps a
    /// test's writes inside its own temp directory.
    Under(Option<std::path::PathBuf>),
}

/// How often a session that cannot reach the store rewrites its buffer.
///
/// The export grows with everything typed since the last acknowledgement, so
/// doing it on every turn would pay a lengthening cost several times a second
/// for as long as the store is away. A second is far inside what a person could
/// lose by killing the process, and the flush on the way out is not throttled
/// at all.
const BUFFER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How many refusals in a row before a session stops rather than resending.
///
/// A rewind re-exports everything since the last confirmed point, so a refusal
/// the store will *always* refuse — an update it considers invalid — becomes an
/// infinite resend without this. Three, because one is a blip and two is bad
/// luck; the same judgement `Session::write_failed` makes about disk writes.
const REFUSALS_BEFORE_STOPPING: u32 = 3;

impl StoreSession {
    /// Join a document, and return the snapshot to open it from.
    ///
    /// **The snapshot comes over HTTP, not out of the join.** Joining a room
    /// answers with a version vector and, if this replica is behind, a catch-up
    /// delta — not with the document. A client with nothing yet needs the whole
    /// state, and `Client::snapshot` is where that lives. Getting this backwards
    /// is the trap Loro makes silent: a delta whose causal ancestors are missing
    /// imports `Ok` and applies nothing.
    pub fn join(
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<(Self, Vec<u8>), koru_store_core::errors::Error> {
        // A session can remain open for days. Keep the rendezvous location on
        // its Store client so it can replace a spent bearer credential itself
        // instead of surfacing an authentication failure as a missing page.
        let client = match root {
            Some(root) => Client::open_at(segment, root),
            None => Client::open(segment),
        }?;
        let snapshot = client.snapshot(workspace, document)?;

        let link = Arc::new(ThreadLink::open(segment, root)?);
        let room = document_room(document);
        // An empty version vector, because the snapshot above is what this
        // replica knows and the store is the one deciding what is owed. Sending
        // a *populated* vector this client cannot back up with history is the
        // way to be handed a delta that applies to nothing.
        link.join(&room, &loro::VersionVector::new())?;

        Ok((
            Self::over(link, client, workspace, document, room).buffering_into(root),
            snapshot,
        ))
    }

    /// Join through an explicitly supplied product-client boundary.
    ///
    /// This is the non-discovery seam for embedded and hermetic hosts. The
    /// factory is retained by the socket thread so a Store restart obtains a
    /// new endpoint and credential from the same owning boundary.
    pub fn join_with_client_factory(
        factory: ClientFactory,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<(Self, Vec<u8>), Error> {
        let client = factory()?;
        let snapshot = client.snapshot(workspace, document)?;
        let link = Arc::new(ThreadLink::open_with_client_factory(factory)?);
        let room = document_room(document);
        link.join(&room, &loro::VersionVector::new())?;
        Ok((
            Self::over(link, client, workspace, document, room),
            snapshot,
        ))
    }

    /// A session over any link, which is what makes the failure paths testable.
    ///
    /// Not public: a caller outside this module has no business choosing a
    /// transport, and the two `join` constructors are the whole of the API.
    fn over(
        link: Arc<dyn Link>,
        client: Client,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        room: String,
    ) -> Self {
        Self {
            client,
            link,
            workspace,
            document,
            room,
            sent: None,
            acked: None,
            durable: HashSet::new(),
            inflight: std::collections::VecDeque::new(),
            rejoin: false,
            stopped: None,
            refusals: 0,
            outbox: Buffering::Off,
            buffered: None,
            buffer_path: None,
            buffered_at: None,
            buffer_failure: None,
            replayed: Vec::new(),
            replayed_covered: None,
            replay_reported: false,
        }
    }

    /// A system-session test room over the same fake link as the StoreSession
    /// bookkeeping tests. Kept here so the fake transport remains private to
    /// the store module while the injected system opener can exercise it.
    #[cfg(test)]
    pub(crate) fn for_system_test(
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> (Self, Arc<link::fake::FakeLink>) {
        let link = Arc::new(link::fake::FakeLink::new());
        let client = Client::new(koru_api_client::Discovery {
            port: 0,
            token: String::new(),
            expires_at_millis: i64::MAX,
        });
        let session = Self::over(
            link.clone(),
            client,
            workspace,
            document,
            document_room(document),
        );
        (session, link)
    }

    /// Buffer what the store has not confirmed under this root.
    ///
    /// Separate from [`over`](Self::over) so a session over a fake link writes
    /// nothing by default: a test that has not asked for a disk must not get
    /// one, and `Off` is the honest state for a session with nowhere to write.
    fn buffering_into(mut self, root: Option<&std::path::Path>) -> Self {
        self.outbox = Buffering::Under(root.map(|r| r.to_path_buf()));
        self
    }

    /// Join a workspace's **manifest** — its list of documents — rather than one
    /// document's content.
    ///
    /// The same session type, because it is the same job: a `LoroDoc` behind a
    /// room, exported and imported the same way. What differs is only which
    /// room, and what the bytes mean once they arrive.
    ///
    /// **The manifest is not fetched by `Client::snapshot`.** That route takes a
    /// `DocumentId`, and a manifest is not one of the workspace's documents —
    /// `Client::manifest(workspace)` answers with the id of the document that
    /// *is* the manifest, and that is what gets fetched. Getting this wrong
    /// gives a `DocumentNotFound` for a workspace that is perfectly healthy.
    pub fn join_manifest(
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: StoreWorkspaceId,
    ) -> Result<(Self, Vec<u8>), koru_store_core::errors::Error> {
        // As above, the client owns session renewal for the life of this
        // manifest link rather than freezing the credential from this join.
        let client = match root {
            Some(root) => Client::open_at(segment, root),
            None => Client::open(segment),
        }?;
        let document = client.manifest(workspace)?;
        let snapshot = client.snapshot(workspace, document)?;

        let link = Arc::new(ThreadLink::open(segment, root)?);
        let room = manifest_room(workspace);
        link.join(&room, &loro::VersionVector::new())?;

        Ok((
            Self::over(link, client, workspace, document, room).buffering_into(root),
            snapshot,
        ))
    }

    /// Hand the store everything written since the last call.
    ///
    /// Returns the batch id when something was sent, `None` when there was
    /// nothing to say.
    ///
    /// **Version vectors decide, never `is_empty`.** An update covering no
    /// operations still carries a header, so it is never an empty `Vec` — a
    /// send guarded on `!bytes.is_empty()` sends a pointless frame on every
    /// poll forever. Both this and the trap in [`join`](Self::join) are
    /// recorded in AGENTS.md as Loro traps the store re-learned; they are
    /// re-learnable here too.
    ///
    /// ## A remote update is echoed back, on purpose, for now
    ///
    /// After [`drain`](Self::drain) imports, `doc.version()` has moved, so the
    /// next publish exports those operations and sends the store its own bytes
    /// back. One extra frame per remote edit, over a loopback socket, applied
    /// idempotently at the other end.
    ///
    /// **The obvious cure is a data-loss bug**, which is why it is not here.
    /// Setting `sent = doc.version()` after an import claims the store has
    /// everything this replica holds — and that includes any operation written
    /// *since the last publish*, which nothing has sent and nothing then ever
    /// would. The version to advance to is `sent` merged with the imported
    /// operations alone, and separating those from local ones inside a
    /// `VersionVector` is fiddly enough that getting it wrong is likelier than
    /// the bandwidth mattering. Left as a measured cost — `syncstats`'s `out`
    /// against its `in` is exactly this — rather than a clever fix.
    pub fn publish<T: Synced + ?Sized>(
        &mut self,
        doc: &T,
    ) -> Result<Option<BatchId>, koru_store_core::errors::Error> {
        if let Some(why) = &self.stopped {
            return Err(koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::StorageFailed,
                why.clone(),
            ));
        }
        // **A rejoin happens here because this is the only place holding both
        // the flag and the document.** It has to name what this replica
        // actually holds — anything else and the store computes a catch-up from
        // a position we cannot back up with history, which imports `Ok` and
        // applies nothing. That is the opposite of the empty vector `join` uses,
        // where the replica genuinely holds nothing yet.
        if self.rejoin {
            self.link.join(&self.room, &doc.version())?;
            self.rejoin = false;
        }
        let now = doc.version();
        if self.sent.as_ref() == Some(&now) {
            return Ok(None);
        }
        let update = match &self.sent {
            Some(from) => doc.export_updates_since(from),
            None => doc.export_all_updates(),
        }
        .map_err(|e| {
            koru_store_core::errors::Error::new(
                koru_store_core::errors::Code::CrdtRejected,
                format!("export: {e}"),
            )
        })?;

        // Measured before the move and recorded after the send, so a refused
        // write is not counted as bytes that went out.
        let size = update.len();
        let batch = self.link.send_update(&self.room, update)?;
        koru_api_client::sync_debug!(
            "at_millis={} direction=local_to_store stage=update_sent room={} batch={batch:?} bytes={size}",
            sync_wall_time_millis(),
            self.room,
        );
        crate::syncstats::published(size);
        self.sent = Some(now.clone());
        self.inflight.push_back((batch, now));
        Ok(Some(batch))
    }

    /// Take whatever the store has to say, applying any updates to `doc`.
    ///
    /// Returns whether the document changed, which is what the event loop needs
    /// in order to decide whether to draw.
    pub fn drain<T: Synced + ?Sized>(
        &mut self,
        doc: &mut T,
    ) -> Result<bool, koru_store_core::errors::Error> {
        // `import_frames` below decodes Loro bytes, and loro-internal's own
        // warnings (an inconsistent tree says "Missing in parent's children")
        // name no document. The span prefixes them with the room, which does.
        let _span = tracing::info_span!("doc", room = %self.room).entered();
        let mut changed = false;
        // Bounded, so a busy room cannot hold the event loop: whatever is left
        // is still there on the next poll. Unbounded draining here would make a
        // remote paste of a thousand rows a frame the window skips.
        //
        // **The failure is kept and reported at the end rather than returned on
        // the spot.** The events after it have already been taken from the link,
        // so an early `return` would drop them — and one of them may be an
        // `Updates` frame, which is content. Loro parks a delta whose
        // predecessor is missing and returns `Ok`, so losing one is silent.
        let mut failed = None;
        for inbound in self.link.take(64)? {
            let event = match inbound {
                Inbound::Event(event) => event,
                // **The connection died, and what was in flight is unknowable.**
                // Rewind to the last version the store confirmed on its disk and
                // let the next publish re-export everything since. Assuming the
                // in-flight batches landed loses them; assuming they did not and
                // rewinding *past* `acked` replays the whole session.
                Inbound::Lost { .. } => {
                    // **Buffer before rewinding.** What was in flight is now
                    // unknowable and `inflight` is about to be cleared, so this
                    // is the last moment anything knows there was work owed.
                    self.buffer_now(doc);
                    self.sent = self.acked.clone();
                    self.inflight.clear();
                    self.rejoin = true;
                    continue;
                }
                // A fresh socket has joined nothing. `publish` is where the
                // rejoin happens, because it is the only place holding both this
                // flag and the document whose version the rejoin has to name.
                Inbound::Ready => {
                    self.rejoin = true;
                    continue;
                }
            };
            match event {
                Event::Updates { updates, .. } => {
                    if !updates.is_empty() {
                        crate::syncstats::imported(updates.len());
                        doc.import_frames(&updates).map_err(|e| {
                            koru_store_core::errors::Error::new(
                                koru_store_core::errors::Code::CrdtRejected,
                                format!("import: {e}"),
                            )
                        })?;
                        changed = true;
                    }
                }
                Event::Acked { ack, .. } => self.record(ack),
                // **A refused join is final.** `Unauthenticated` or
                // `DocumentNotFound` will be refused identically forever, so
                // retrying is this session's old swallow with extra steps.
                Event::JoinRefused { message, .. } => {
                    self.stopped.get_or_insert_with(|| message.clone());
                    failed = failed.or(Some(koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::Unauthenticated,
                        message,
                    )));
                }
                // **`RejoinSuggested` is not a failure — it is an instruction.**
                // It means this client has fallen behind (a message too large to
                // relay was committed without it), and the answer is to rejoin
                // naming what we hold. The code used to be flattened into a
                // string and then swallowed by `tick_store`, so the one room
                // error that is recoverable was indistinguishable from the ones
                // that are not.
                Event::RoomProblem {
                    code: loro_protocol::RoomErrorCode::RejoinSuggested,
                    ..
                } => self.rejoin = true,
                Event::RoomProblem { message, .. } => {
                    self.stopped.get_or_insert_with(|| message.clone());
                    failed = failed.or(Some(koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::StorageFailed,
                        message,
                    )));
                }
                Event::Joined { .. } => {}
            }
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(changed),
        }
    }

    /// Take in what the store said about one batch.
    ///
    /// **The refusal arm is the one that was wrong**, and wrong in the way that
    /// hides: the old body removed the batch from `pending` for *every* status
    /// and inserted into `durable` only for `Ok`. So an `InvalidUpdate` or a
    /// `PayloadTooLarge` dropped those operations from this replica's outbound
    /// stream permanently, nothing ever sent them again, and `unacknowledged()`
    /// then read zero — the failure erased its own evidence.
    fn record(&mut self, ack: Ack) {
        // A batch this session does not recognise: a late answer from a
        // connection that is gone. Ignored rather than acted on, which is what
        // monotonic batch ids are for.
        if !self.inflight.iter().any(|(b, _)| *b == ack.batch) {
            return;
        }
        if ack.durable {
            koru_api_client::sync_debug!(
                "at_millis={} direction=local_to_store stage=update_durable room={} batch={:?} error=-",
                sync_wall_time_millis(),
                self.room,
                ack.batch,
            );
            // **Everything sent before it is confirmed too.** A room answers in
            // send order, so an acknowledgement of batch *n* is an
            // acknowledgement that *n-1* was applied before it.
            while let Some((batch, version)) = self.inflight.pop_front() {
                self.acked = Some(version);
                if batch == ack.batch {
                    break;
                }
            }
            self.durable.insert(ack.batch);
            self.refusals = 0;
            // The store confirmed it, so the copy on this machine has done its
            // job. Left behind, it would be replayed on the next open — which
            // is harmless but never stops happening.
            self.prune_buffer();
            return;
        }
        // Refused. **Rewind to the last confirmed point** so the next publish
        // re-exports everything since it — including whatever this batch
        // carried, which is the part that used to vanish. Sound because `acked`
        // is a version the store committed, so a delta from it has ancestors the
        // store has by definition; the surplus it also carries is idempotent.
        self.sent = self.acked.clone();
        self.inflight.clear();
        // A rate limit is the store asking for patience, not refusing the
        // content — resending is exactly the right answer and it must not count
        // toward giving up.
        if ack.status != loro_protocol::UpdateStatusCode::RateLimited {
            self.refusals += 1;
            if self.refusals >= REFUSALS_BEFORE_STOPPING {
                self.stopped = Some(format!(
                    "the store refused {} updates in a row ({:?})",
                    self.refusals, ack.status
                ));
            }
        }
    }

    // ------------------------------------------------------------- the outbox

    /// Whether this document holds work the store has not confirmed.
    ///
    /// **Not [`unacknowledged`](Self::unacknowledged), and the difference
    /// matters.** That counts batches in flight, and a lost connection clears
    /// them while rewinding `sent` — so it reads zero at the exact moment the
    /// most is at stake. This asks the question a person is actually asking:
    /// has everything I typed reached the store's disk.
    pub fn owes<T: Synced + ?Sized>(&self, doc: &T) -> bool {
        !self
            .acked
            .clone()
            .unwrap_or_default()
            .includes_vv(&doc.version())
    }

    /// Whether this machine's own copy reaches everything the document holds.
    ///
    /// **The third state, and the only one a person can act on.** [`owes`] says
    /// the store has not confirmed; this says whether the work is nevertheless
    /// on a disk, so closing the window costs nothing. The two together are the
    /// difference between *saved somewhere else in a moment* and *saved
    /// nowhere*.
    ///
    /// Compared with `includes_vv` against the document's version **now**,
    /// rather than by asking whether a file exists: [`buffer_now`] throttles to
    /// once a second, so a file written a moment ago may cover less than has
    /// been typed since. A stale file reported as cover would be exactly the
    /// reassuring lie this feature exists to avoid.
    ///
    /// [`owes`]: Self::owes
    /// [`buffer_now`]: Self::buffer_now
    pub fn buffered_covers<T: Synced + ?Sized>(&self, doc: &T) -> bool {
        self.buffered
            .as_ref()
            .is_some_and(|buffered| buffered.includes_vv(&doc.version()))
    }

    /// Why the last buffer attempt wrote nothing, if it wrote nothing.
    pub fn buffer_failure(&self) -> Option<&str> {
        self.buffer_failure.as_deref()
    }

    /// Put what the store has not confirmed on this machine's disk — at most
    /// once every [`BUFFER_INTERVAL`], because the export lengthens with
    /// everything typed since the last acknowledgement.
    pub fn buffer_if_due<T: Synced + ?Sized>(&mut self, doc: &T) {
        if self
            .buffered_at
            .is_some_and(|at| at.elapsed() < BUFFER_INTERVAL)
        {
            return;
        }
        self.buffer_now(doc);
    }

    /// The same, unthrottled — on the way out, and before a session is dropped.
    ///
    /// A tab closing drops its session, so a document typed into and then
    /// closed would otherwise lose everything since the last acknowledgement
    /// before any exit path ran.
    pub fn buffer_now<T: Synced + ?Sized>(&mut self, doc: &T) {
        let Buffering::Under(root) = &self.outbox else {
            return;
        };
        let version = doc.version();
        let acked = self.acked.clone().unwrap_or_default();
        if acked.includes_vv(&version) {
            // The store has it all. Anything on the disk is now delivered.
            self.discard_buffer();
            return;
        }
        if self.buffered.as_ref() == Some(&version) {
            // Already written, and the document has not moved since.
            return;
        }

        self.buffered_at = Some(std::time::Instant::now());
        let updates = match doc.export_updates_since(&acked) {
            Ok(bytes) => bytes,
            Err(why) => {
                self.buffer_failure = Some(format!("export: {why}"));
                return;
            }
        };
        let root = root.clone();
        match outbox::write(
            root.as_deref(),
            self.workspace,
            self.document,
            doc.peer_id(),
            &acked,
            &updates,
        ) {
            Ok(path) => {
                self.buffered = Some(version);
                self.buffer_path = Some(path);
                self.buffer_failure = None;
            }
            Err(why) => self.buffer_failure = Some(why.to_string()),
        }
    }

    /// Take back anything this machine buffered for this document.
    ///
    /// Called once, with the document the store has just handed over, before
    /// anything is published. Returns what it could **not** take back, in words
    /// — a buffer that is skipped silently is somebody's work disappearing at
    /// the one moment it could still have been saved.
    ///
    /// **Every file under the document is replayed, whoever wrote it.** The
    /// payload is opaque update bytes addressed to this document, Loro merges,
    /// and importing an update twice is a no-op — so a window that finds another
    /// program's buffer delivers it rather than stepping around it.
    ///
    /// Nothing is deleted here. The files go on
    /// [`replayed`](Self::replayed) and are removed only once the store
    /// acknowledges a version that covers them.
    ///
    /// **It says what it did, on stderr, and not only when asked** — but only
    /// when it did something. Work that was buffered because the store could
    /// not be reached is the one thing in this system a person is entitled to
    /// see accounted for, and a diagnostic switch would mean it was off on the
    /// run that mattered.
    ///
    /// **A file that applies nothing is not news.** A buffer is removed only
    /// once the store acknowledges a version covering it, so after a reconnect
    /// the live link usually delivers the work first and leaves the file behind
    /// as a duplicate — which is then replayed, applies zero operations, and is
    /// pruned. That is the *ordinary* path, once per document as each is
    /// opened, and reporting it buried the one case worth reading in three
    /// lines of nothing per document. Reported now: operations actually
    /// recovered, and anything that could not be applied.
    pub fn replay_buffered<T: Synced + ?Sized>(&mut self, doc: &mut T) -> Vec<String> {
        // Same reason as `drain`'s span: the imports below decode Loro bytes,
        // and the dependency's warnings name no document on their own.
        let _span =
            tracing::info_span!("doc", workspace = %self.workspace, document = %self.document)
                .entered();
        let Buffering::Under(root) = &self.outbox else {
            return Vec::new();
        };
        let root = root.clone();
        let (found, refused) = outbox::read_all(root.as_deref(), self.workspace, self.document);
        let mut complaints: Vec<String> = refused.into_iter().map(|e| e.0).collect();
        let files = found.len() + complaints.len();
        let bytes: usize = found.iter().map(|b| b.updates.len()).sum();
        // Collected rather than printed as they go, because whether any of this
        // is worth a line is only known once every file has been tried.
        let mut applied: Vec<(String, i64)> = Vec::new();

        for buffered in found {
            // **The guard that keeps this honest.** A delta whose causal
            // ancestors are missing imports `Ok` and applies nothing, so
            // importing one that cannot apply would look like delivery and be
            // loss. The file stays where it is.
            if !buffered.applies_to(&doc.version()) {
                complaints.push(format!(
                    "{}: the store's copy does not reach back to where these changes start, \
                     so they have been left on this machine rather than applied",
                    buffered.path.display()
                ));
                continue;
            }
            let name = file_name(&buffered.path);
            let before = doc.version();
            match doc.import_frames(&[buffered.updates]) {
                Ok(()) => {
                    // **The number a person can act on.** Bytes say how big the
                    // file was; this says how much of it the store had never
                    // seen — and a replay of a file the store already had is a
                    // legitimate zero, not a failure. Loro counts an operation
                    // per peer, so the difference between the two version
                    // vectors *is* the count.
                    applied.push((name, ops_between(&before, &doc.version())));
                    self.replayed.push(buffered.path);
                }
                Err(why) => complaints.push(format!("{}: {why}", buffered.path.display())),
            }
        }
        let recovered: i64 = applied.iter().map(|(_, ops)| ops).sum();
        if recovered > 0 || !complaints.is_empty() {
            eprintln!(
                "[Koru] outbox: {files} file(s), {bytes} bytes waiting for document {} — replaying",
                self.document
            );
            for (name, ops) in applied.iter().filter(|(_, ops)| *ops > 0) {
                eprintln!("[Koru] outbox:   {name} — {ops} op(s) applied");
            }
            for complaint in &complaints {
                eprintln!("[Koru] outbox:   NOT applied — {complaint}");
            }
            // What licenses the confirmation line below to speak: without this,
            // the pruning of a duplicate nobody was told about announced itself.
            self.replay_reported = true;
        }
        if !self.replayed.is_empty() {
            // After the imports, this covers everything they carried — which is
            // what an acknowledgement has to reach before the files can go.
            self.replayed_covered = Some(doc.version());
        }
        complaints
    }

    /// Forget the buffer, because what it held is on the store's disk.
    fn discard_buffer(&mut self) {
        if let Some(path) = self.buffer_path.take() {
            outbox::remove(&path);
        }
        self.buffered = None;
        self.buffer_failure = None;
    }

    /// Drop what the last acknowledgement covered — this session's own buffer,
    /// and anything it replayed on the way in.
    ///
    /// Compared with `includes_vv` rather than by equality, because the store
    /// may have confirmed *more* than a file held — another window's work
    /// through the same room, most obviously.
    fn prune_buffer(&mut self) {
        let Some(acked) = self.acked.clone() else {
            return;
        };
        if self.buffered.as_ref().is_some_and(|b| acked.includes_vv(b)) {
            self.discard_buffer();
        }
        if self
            .replayed_covered
            .as_ref()
            .is_some_and(|covered| acked.includes_vv(covered))
        {
            // **The other half of the account, and only of an account that was
            // opened.** "Replaying" without a matching "the store has it"
            // leaves somebody watching a log that told them work was in flight
            // and never told them it landed. The converse is just as bad and is
            // what shipped first: a confirmation for a duplicate file nobody
            // was told about, once per document opened.
            if !self.replayed.is_empty() && self.replay_reported {
                eprintln!(
                    "[Koru] outbox: the store has confirmed everything replayed for document {} \
                     — {} file(s) removed",
                    self.document,
                    self.replayed.len()
                );
            }
            for path in self.replayed.drain(..) {
                outbox::remove(&path);
            }
            self.replayed_covered = None;
        }
    }

    /// Whether the store has said this batch is on the disk.
    pub fn is_durable(&self, batch: BatchId) -> bool {
        self.durable.contains(&batch)
    }

    /// Batches sent and not yet answered — what would be lost if this process
    /// died now.
    pub fn unacknowledged(&self) -> usize {
        self.inflight.len()
    }

    /// Leave the room, on the way out.
    ///
    /// Best-effort by design: the store drops a connection's rooms when the
    /// connection goes, so this only saves it from relaying to somebody who has
    /// already stopped listening. A failure here means the socket is already
    /// gone, which is the outcome anyway.
    pub fn leave(&self) {
        let _ = self.link.leave(&self.room);
    }

    /// Whether this session's connection is getting through.
    pub fn link_state(&self) -> link::LinkState {
        self.link.state()
    }

    /// How this session should wake whoever drains it.
    pub fn set_waker(&self, waker: link::Waker) {
        self.link.set_waker(waker);
    }

    /// Why this session has given up, if it has. `None` while it is working.
    ///
    /// A session that has stopped is not one that will quietly come back: it has
    /// been refused a join, told about a room problem it cannot recover from, or
    /// had several updates in a row rejected. Everything typed since is in this
    /// window and nowhere else, which is what makes this worth surfacing rather
    /// than logging.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    pub fn workspace(&self) -> StoreWorkspaceId {
        self.workspace
    }

    pub fn document(&self) -> StoreDocumentId {
        self.document
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

fn sync_wall_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// How many operations `after` holds that `before` did not.
///
/// **A version vector is a counter per peer**, so the difference between two of
/// them is the number of operations between them — which is what makes "20 ops
/// applied" answerable without decoding the payload the outbox carries. Peers
/// that only `before` knows about are ignored rather than subtracted: an import
/// cannot take operations away, and a negative would be arithmetic showing
/// through rather than an answer.
fn ops_between(before: &loro::VersionVector, after: &loro::VersionVector) -> i64 {
    after
        .iter()
        .map(|(peer, counter)| {
            let had = before.get(peer).copied().unwrap_or(0);
            i64::from((*counter - had).max(0))
        })
        .sum()
}

/// The file's own name, for a log line that has to fit on one.
///
/// The full path is `<store root>/outbox/<workspace>/<document>/<replica>.loro`
/// and the two ids are already on the line above it.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A workspace's blobs, kept by the store.
///
/// The counterpart of `koru_docstore::BlobStore`, which files bytes under a hash
/// in a folder. Same two operations, different keeper.
///
/// ## The app stops hashing
///
/// `koru-docstore` computes FNV-1a and files at `fnv1a:<32 hex>`; the store
/// computes BLAKE3 and files at `b3:<64 hex>`. Those are two addresses for one
/// blob, and content addressing only works if both sides agree on the address.
///
/// So the app does not compute one at all any more: [`put`](Self::put) hands
/// over the bytes and **records the hash the store gives back**. A document's
/// `Content::Blob` holds whatever string that is, which it always could —
/// `koru_core::node::BlobHash` is a `String` newtype and was never tied to an
/// algorithm.
///
/// The alternative was keeping FNV-1a app-side and mapping between the two,
/// which is a second index that can disagree; the failure it produces is an
/// image that renders on one machine and is a grey box on another, with nothing
/// logged. Existing FNV-addressed images are not migrated in place — an old
/// workspace arrives through *Import old workspace…*, which re-puts its bytes
/// and gets store addresses for them.
#[derive(Clone)]
pub struct StoreBlobs {
    client: Client,
    workspace: StoreWorkspaceId,
}

impl StoreBlobs {
    pub fn new(client: Client, workspace: StoreWorkspaceId) -> Self {
        Self { client, workspace }
    }

    /// Store bytes and return the address they are filed under.
    ///
    /// Idempotent by construction: the same bytes are the same address, so a
    /// picture pasted twice costs one copy.
    pub fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_core::node::BlobHash, koru_store_core::errors::Error> {
        let blob = self
            .client
            .put_blob(self.workspace, bytes, mime, filename)?;
        Ok(koru_core::node::BlobHash(blob.hash.as_str().to_owned()))
    }

    /// The bytes behind an address, or an error if this workspace has none.
    pub fn get(
        &self,
        hash: &koru_core::node::BlobHash,
    ) -> Result<Vec<u8>, koru_store_core::errors::Error> {
        let parsed = koru_store_core::model::BlobHash::parse(&hash.0)?;
        self.client.get_blob(self.workspace, &parsed)
    }

    /// Whether this workspace holds an address already.
    ///
    /// A `get` that throws its bytes away, deliberately: the store has no
    /// "exists" route, and inventing one to save a round trip nobody has
    /// measured would be a route to keep working forever.
    pub fn contains(&self, hash: &koru_core::node::BlobHash) -> bool {
        self.get(hash).is_ok()
    }
}

#[cfg(test)]
mod tests;
