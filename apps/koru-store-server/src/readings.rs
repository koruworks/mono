// SPDX-License-Identifier: MPL-2.0

//! **Reading the words out of files, inside the Store daemon** — the
//! coordinator, the readings mirror, and the source the search index asks
//! (OCR TECHNICAL D11, D14, §7, §9, §14).
//!
//! ```text
//!  a file's entry projected ─┐     bytes land ─┐ (Store::take_blobs_stored)
//!                            ▼                 ▼
//!                       ┌──────── Inbox ────────┐
//!                       ▼                       ▼
//!   coordinator thread: the queue (a query) → one job → launcher → worker
//!                       │  ensure_reading, ReadingDoc, apply + flush (Store origin)
//!                       ▼
//!   a commit to a reading document ── tee in StoreSource::take_committed ──┐
//!   (made here, or arrived from a peer or Koru Cloud)                      ▼
//!   mirror thread: fork that one document, upsert readings.db, wake search ─┘
//!                       │                          catch-up at start and on
//!                       ▼                          every listing change
//!   koru-search's Readings ← readings.db (its own connection)
//! ```
//!
//! **Two threads, and neither answers anybody.** The coordinator may block on
//! a worker for as long as a file takes; the mirror on a fork and a
//! transaction. Both take the Store lock only to fork, register or apply, the
//! way `systemdoc.rs` and `tagnames.rs` do. A thread that answers a person
//! never waits on either: the index reads `readings.db` through its own
//! connection, and the debug route reads the JSON these threads publish.
//!
//! **The daemon writes one kind of document's content** — this one. That is
//! a narrow exception to ADR-80, recorded in its own decision record; the
//! shape is `koru-read`'s, and `koru-store-core` stays opaque.

pub(crate) mod applier;
pub(crate) mod coordinator;
mod corrections;
pub(crate) mod feed;
pub(crate) mod launcher;
pub(crate) mod mirror;
pub(crate) mod power;
pub(crate) mod supervised;
// The reader that finds words in pictures: its installer and the state the
// installer, the coordinator, search and the routes share (OCR Plan 2.4).
pub(crate) mod hub;
pub(crate) mod installer;
// The items behind the Settings row's could-not-be-read and partly-read lists
// (OCR Plans 2.5 and 3.5).
pub(crate) mod items;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use koru_store_api::state::AppState;
use koru_store_core::ids::WorkspaceId;

pub use feed::{Inbox, ReadingsFeed, Tee};
pub use hub::ReaderHub;
pub use installer::InstallerConfig;
pub use launcher::Launcher;

use crate::derived::Derived;

/// How long the mirror's thread waits for work before looking at its stop
/// flag. **Woken by its inbox, not by this**: no work starts because it
/// elapsed.
const IDLE: Duration = Duration::from_millis(250);

/// Whether a Store host runs the read worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderLaunch {
    /// No reader: a phone, an in-memory composition, the integration host.
    /// The mirror still runs; nothing is read here.
    None,
    /// Supervise `koru-read-worker` beside this executable.
    Sibling,
}

/// What has been opened but not yet started: the inbox the projector's tee
/// and consumer push into, before the projector's first tick.
pub struct Prepared {
    inbox: Arc<Inbox>,
    derived: Derived,
    hub: Arc<ReaderHub>,
}

/// Open the mirror and build the inbox.
///
/// **Not fatal**, as the index's `prepare` is not: a mirror that will not open
/// is a store whose files are found by name only, not one that has lost
/// anything.
pub fn prepare(derived: &Derived) -> Option<Prepared> {
    match derived.mirror() {
        Ok(_) => Some(Prepared {
            inbox: Inbox::new(),
            derived: derived.clone(),
            hub: ReaderHub::new(),
        }),
        Err(error) => {
            tracing::error!("the readings mirror did not open: {error}");
            None
        }
    }
}

impl Prepared {
    /// Registered with the projector beside the search feed.
    pub fn consumer(&self) -> Box<dyn koru_projection::Consumer> {
        Box::new(ReadingsFeed::new(Arc::clone(&self.inbox)))
    }

    /// Handed to `StoreSource`, which drains commits and landed bytes.
    pub fn tee(&self) -> Tee {
        Tee(Arc::clone(&self.inbox))
    }

    /// The index's source of words, on a connection of its own.
    pub fn source(&self) -> Option<Box<dyn koru_search::Readings>> {
        match self.derived.mirror() {
            Ok(mirror) => Some(Box::new(MirrorReadings(Mutex::new(mirror)))),
            Err(error) => {
                tracing::warn!("search will find files by name only: {error}");
                None
            }
        }
    }

    pub fn inbox(&self) -> Arc<Inbox> {
        Arc::clone(&self.inbox)
    }

    /// The reader's shared state: what the search answer reads its
    /// `reading` counts from (OCR Plan 2.4). A read of memory.
    pub fn hub(&self) -> Arc<ReaderHub> {
        Arc::clone(&self.hub)
    }

    /// Start the mirror's thread and, with a launcher, the coordinator's.
    ///
    /// `objects` is where the worker opens bytes from; a host with no file
    /// bytes (an in-memory composition) passes none and runs no coordinator.
    /// `installer` is where the reader is installed (OCR Plan 2.4); a host
    /// that passes none still answers `/v1/readings/reader`, saying plainly
    /// that no reader can be used there.
    pub fn spawn(
        self,
        state: &Arc<AppState>,
        search: Option<koru_search::Queue>,
        launcher: Option<Box<dyn Launcher>>,
        objects: Option<Box<dyn coordinator::Objects>>,
        installer: Option<InstallerConfig>,
        stop: Arc<AtomicBool>,
    ) -> Vec<std::thread::JoinHandle<()>> {
        let board = Arc::new(Board::new(state));
        let mut threads = Vec::new();
        // Where the bytes are: the coordinator's, and the readings route's own
        // question before it accepts a request (OCR Plan 3.2).
        let objects: Option<Arc<dyn coordinator::Objects>> = objects.map(Arc::from);

        // The reader's installer, before the coordinator, so the model it
        // finds on the disk is what the first worker is launched with.
        let service: Arc<dyn koru_store_api::routes::reader::ReaderService> = match installer {
            Some(config) if launcher.is_some() && objects.is_some() => {
                // The reader installed or removed goes to the event bus the
                // way the Store's other committed facts do (OCR TECHNICAL
                // §10): through the daemon's catalog-fact sink, which is
                // silent where no events worker is wired.
                let weak = Arc::downgrade(state);
                let announce: installer::Announce = Box::new(move |draft| {
                    if let Some(state) = weak.upgrade() {
                        state.publish_catalog_event(draft);
                    }
                });
                let (service, handle) = installer::start(
                    config,
                    Arc::clone(&self.hub),
                    Arc::clone(&self.inbox),
                    Some(announce),
                    stop.clone(),
                );
                if let Some(handle) = handle {
                    threads.push(handle);
                }
                Arc::new(service)
            }
            _ => Arc::new(installer::Installer::idle(Arc::clone(&self.hub))),
        };
        let _ = state.set_reader_service(service);

        // `GET /v1/readings/{workspace}/{hash}`, on a mirror connection of its
        // own so a request never waits on the applier's transaction.
        match self.derived.mirror() {
            Ok(mirror) => {
                let port = ReadingPort {
                    state: Arc::downgrade(state),
                    mirror: Mutex::new(mirror),
                    hub: Arc::clone(&self.hub),
                    inbox: Arc::clone(&self.inbox),
                    // Requests are met only where a coordinator runs.
                    objects: objects.clone().filter(|_| launcher.is_some()),
                    items: items::ItemsSource::new(self.derived.clone()),
                };
                if !state.set_reading_service(Arc::new(port)) {
                    tracing::warn!("the readings port was already set; the first one stays");
                }
            }
            Err(error) => tracing::warn!("one reading cannot be asked for: {error}"),
        }

        match self.derived.mirror() {
            Ok(mirror) => {
                let applier = applier::Applier::new(
                    state,
                    mirror,
                    self.derived.projections().ok(),
                    Arc::clone(&self.inbox),
                    search,
                );
                if let Some(handle) = spawn_mirror(
                    applier,
                    Arc::clone(&self.inbox),
                    Arc::clone(&board),
                    stop.clone(),
                ) {
                    threads.push(handle);
                }
            }
            Err(error) => tracing::error!("the readings mirror did not start: {error}"),
        }

        let (Some(launcher), Some(objects)) = (launcher, objects) else {
            board.coordinator(serde_json::json!({
                "running": false,
                "worker": launcher::NoLauncher.report(),
            }));
            return threads;
        };
        let (Ok(projections), Ok(mirror)) = (self.derived.projections(), self.derived.mirror())
        else {
            tracing::error!("the reading coordinator has no read model to query; nothing is read");
            return threads;
        };
        let mut coordinator = coordinator::Coordinator::shared(
            state,
            projections,
            mirror,
            launcher,
            objects,
            Arc::clone(&self.inbox),
        )
        .with_reader(Arc::clone(&self.hub))
        .with_power(Box::new(power::SystemPower));
        board.set_current(Arc::clone(&coordinator.current));
        let progress = Arc::clone(&board);
        coordinator.on_progress(move || progress.publish());
        if let Some(handle) = spawn_coordinator(coordinator, self.inbox, board, stop) {
            threads.push(handle);
        }
        threads
    }
}

/// The launcher a host's [`ReaderLaunch`] asks for, over a store root.
pub fn launcher_for(launch: ReaderLaunch, root: &Path, channel: &str) -> Option<Box<dyn Launcher>> {
    match launch {
        ReaderLaunch::None => None,
        ReaderLaunch::Sibling => Some(Box::new(supervised::SupervisedReader::new(root, channel))),
    }
}

/// A device's blob objects, under its store root.
pub fn objects_under(root: &Path) -> Box<dyn coordinator::Objects> {
    Box::new(coordinator::ObjectDir(objects_dir(root)))
}

/// `<root>/blobs/objects`, where the verified objects are filed by hash.
///
/// **The blob store's own answer, not the backend's.** `SqliteBackend::
/// objects_dir` is the blob store's *root* (`<root>/blobs`, which also holds
/// `staging`); the objects are one level down. Reading from the first made
/// every file count `not_here` on a real store root — no stub launcher test
/// could see it, because the stubs never open a path.
fn objects_dir(root: &Path) -> PathBuf {
    koru_store_sqlite::SqliteBackend::new(root)
        .objects()
        .objects_dir()
}

fn spawn_mirror(
    mut applier: applier::Applier,
    inbox: Arc<Inbox>,
    board: Arc<Board>,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("koru-readings-mirror".into())
        .spawn(move || {
            board.mirror(mirror_report(&applier, &inbox));
            while !stop.load(Ordering::Acquire) && !inbox.is_stopping() {
                if applier.step() {
                    board.mirror(mirror_report(&applier, &inbox));
                } else {
                    inbox.wait_for_mirror(IDLE);
                }
            }
        })
        .ok()
}

fn spawn_coordinator(
    mut coordinator: coordinator::Coordinator,
    inbox: Arc<Inbox>,
    board: Arc<Board>,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    // A committed reading wakes the desktop's replication runtime, whose
    // notifier spawns async work. Native threads do not inherit Tokio's
    // context. Keep the Store's runtime entered while this thread runs;
    // in-memory hosts without one have no async replication notifier.
    let runtime = tokio::runtime::Handle::try_current().ok();
    let handle = std::thread::Builder::new()
        .name("koru-readings".into())
        .spawn(move || {
            let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
            while !stop.load(Ordering::Acquire) && !inbox.is_stopping() {
                let now = koru_store_core::model::Timestamp::now().millis();
                let busy = coordinator.step(now);
                board.coordinator(coordinator_report(&coordinator));
                if !busy && !inbox.wait_for_coordinator(supervised::LINGER) {
                    // Nothing arrived while it lingered: let the worker go, so
                    // its own idle exit is never what ends it (see
                    // `supervised.rs`). The next job starts a fresh one.
                    coordinator.launcher().idle();
                    board.coordinator(coordinator_report(&coordinator));
                }
            }
        })
        .ok();
    if handle.is_some() {
        tracing::info!("readings: coordinating");
    }
    handle
}

/// **One reading, for `GET /v1/readings/{workspace}/{hash}`** (TECHNICAL
/// §10).
///
/// The mirror says which entry's reading wins in the workspace — the one the
/// index finds the bytes by — and holds its words. It does not keep the
/// reader, the confidence or how far a partial reading got, so those are read
/// from that entry's reading document: one fork under the Store lock, decoded
/// outside it, as the applier does. If the document cannot be forked (not yet
/// on this device, or the Store is going away) the mirror's row and words are
/// the answer, with those three absent.
struct ReadingPort {
    state: Weak<AppState>,
    mirror: Mutex<mirror::Mirror>,
    /// Whether the reader is installed, for the requests (OCR Plan 3.2).
    hub: Arc<ReaderHub>,
    /// Wakes the coordinator when a request is recorded.
    inbox: Arc<Inbox>,
    /// Where the bytes are; `None` on a host that reads nothing, which then
    /// offers and accepts no request.
    objects: Option<Arc<dyn coordinator::Objects>>,
    /// `GET /v1/readings/reader/items`, on connections of its own, so a long
    /// list never holds up one reading.
    items: items::ItemsSource,
}

impl ReadingPort {
    /// The winning row, its words, and the entry it comes from.
    #[allow(clippy::type_complexity)]
    fn winner(
        &self,
        workspace: WorkspaceId,
        hash: &str,
    ) -> Result<
        Option<(
            koru_store_core::ids::DocumentId,
            mirror::ReadingRow,
            Option<koru_read::Reading>,
            Vec<(i64, String)>,
            Facts,
            Option<String>,
        )>,
        String,
    > {
        let (row, words, pending, failed_here) = {
            let mirror = self
                .mirror
                .lock()
                .map_err(|_| "the readings mirror's lock is poisoned".to_owned())?;
            let Some(row) = mirror.winner_in(workspace, hash).map_err(|e| e.0)? else {
                return Ok(None);
            };
            let words = mirror.pages_of(&row.workspace, hash).map_err(|e| e.0)?;
            let pending = mirror.requested(workspace, hash).map_err(|e| e.0)?;
            // A crash can leave a partial reading behind as well as a local
            // failure: both are true, and the failure is what offers again.
            let failed_here = Self::local_failure(&mirror, hash)?;
            (row, words, pending, failed_here)
        };
        let parent = koru_store_core::ids::DocumentId::parse(&row.parent)
            .ok_or_else(|| "the readings mirror holds a malformed document id".to_owned())?;
        // One Store lock: the entry's fork and the Workspace's blob row.
        let (fork, mime) = match self.state.upgrade() {
            Some(state) => match state.store.lock() {
                Ok(mut store) => (
                    store
                        .fork_document(workspace, koru_store_core::ids::reading_document_id(parent))
                        .ok(),
                    koru_store_core::model::BlobHash::parse(hash)
                        .ok()
                        .and_then(|blob| store.blob(workspace, &blob).ok())
                        .map(|blob| blob.mime),
                ),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        let entry = fork.and_then(|fork| koru_read::ReadingDoc::from_doc(fork).reading(hash));
        // The disk, outside every lock.
        let here = mime.is_some()
            && self
                .objects
                .as_ref()
                .is_some_and(|objects| objects.here(hash));
        let facts = Facts {
            status: entry
                .as_ref()
                .and_then(|e| e.status.map(|s| s.as_str().to_owned()))
                .or_else(|| row.status.clone()),
            reason: entry
                .as_ref()
                .map(|e| e.reason.map(|r| r.as_str().to_owned()))
                .unwrap_or_else(|| row.reason.clone()),
            source: entry
                .as_ref()
                .and_then(|e| e.source.map(|s| s.as_str().to_owned()))
                .or_else(|| row.source.clone()),
            reader: entry
                .as_ref()
                .and_then(|e| e.reader.as_ref().map(|r| r.as_str().to_owned())),
            mime,
            here,
            reader_ready: self.reader_ready(),
            pending,
            failed_here: failed_here.is_some(),
        };
        Ok(Some((parent, row, entry, words, facts, failed_here)))
    }

    /// A reader installed and on, that this computer can use.
    fn reader_ready(&self) -> bool {
        self.hub.reading_on() && self.hub.unavailable().is_none()
    }

    /// **The local failure that ended reading `hash` here**, if this computer
    /// gave up on it (OCR Plan 3.5): the attempts table's verdict, by the
    /// coordinator's own [`coordinator::verdict`], so the menus and the
    /// Settings list say *failed* exactly where the counts do.
    fn local_failure(mirror: &mirror::Mirror, hash: &str) -> Result<Option<String>, String> {
        let attempts = mirror.attempts(koru_read::READER_TEXT).map_err(|e| e.0)?;
        let now = koru_store_core::model::Timestamp::now().millis();
        Ok(attempts
            .get(hash)
            .filter(|attempt| coordinator::verdict(attempt, now) == coordinator::Verdict::Failed)
            .map(|attempt| attempt.code.as_str().to_owned()))
    }

    /// A live item without a synced reading: unread, waiting, or failed here.
    /// Resolve its holder before exposing local state or accepting a request.
    fn local_item(
        &self,
        workspace: WorkspaceId,
        hash: &str,
    ) -> Result<Option<(koru_store_core::ids::DocumentId, Facts, Option<String>)>, String> {
        let (code, pending) = {
            let mirror = self
                .mirror
                .lock()
                .map_err(|_| "the readings mirror's lock is poisoned".to_owned())?;
            let code = Self::local_failure(&mirror, hash)?;
            (code, mirror.requested(workspace, hash).map_err(|e| e.0)?)
        };
        let Some(document) = self.items.holder(workspace, hash)? else {
            return Ok(None);
        };
        let mime = self.state.upgrade().and_then(|state| {
            let mut store = state.store.lock().ok()?;
            let blob = koru_store_core::model::BlobHash::parse(hash).ok()?;
            store.blob(workspace, &blob).ok().map(|blob| blob.mime)
        });
        let here = mime.is_some()
            && self
                .objects
                .as_ref()
                .is_some_and(|objects| objects.here(hash));
        let facts = Facts {
            status: None,
            reason: None,
            source: None,
            reader: None,
            mime,
            here,
            reader_ready: self.reader_ready(),
            pending,
            failed_here: code.is_some(),
        };
        Ok(Some((document, facts, code)))
    }
}

impl koru_store_api::routes::readings::ReadingService for ReadingPort {
    fn correct(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        request: koru_store_wire::readings::CorrectReading,
    ) -> Result<koru_store_wire::readings::ReadingCorrections, koru_store_core::errors::Error> {
        self.save_correction(workspace, hash, request)
    }
    fn reading(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Result<Option<koru_store_wire::readings::ReadingAnswer>, String> {
        let hash = hash.as_str();
        if let Some((parent, row, entry, words, facts, failed_here)) =
            self.winner(workspace, hash)?
        {
            let mut answer = answer(workspace, parent, &row, entry, words);
            answer.offers = facts.offers();
            answer.failed_here = failed_here;
            answer.local = Some(facts.local());
            answer.corrections = corrections::wire(
                &self
                    .correction_history(workspace, hash)
                    .map_err(|e| e.to_string())?,
            );
            return Ok(Some(answer));
        }
        // An unread live item still has inspectable local state.
        let corrections = corrections::wire(
            &self
                .correction_history(workspace, hash)
                .map_err(|e| e.to_string())?,
        );
        Ok(self
            .local_item(workspace, hash)?
            .map(|(document, facts, code)| {
                let mut answer = local_answer(workspace, hash, document, code, facts.offers());
                answer.local = Some(facts.local());
                answer.corrections = corrections.clone();
                answer
            }))
    }

    /// **Record a request** (OCR Plan 3.2): the same facts the offers are
    /// made from, so a request is refused exactly where it was not offered.
    ///
    /// *Read again* on an item this computer gave up on (OCR Plan 3.5) also
    /// clears its local attempt record, so the coordinator tries it once
    /// more as if for the first time — with the first time's retries, and no
    /// more: when it fails for good again the request is met and dropped, as
    /// any request whose item fails for good at this generation is
    /// (`Coordinator::classify`), and the item is listed and offered again.
    fn request(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        kind: koru_store_wire::readings::ReadRequestKind,
    ) -> Result<koru_store_wire::readings::ReadRequested, koru_store_core::errors::Error> {
        use koru_store_core::errors::{Code, Error};
        let storage = |error: String| Error::new(Code::StorageFailed, error);
        let facts = match self.winner(workspace, hash.as_str()).map_err(storage)? {
            Some((.., facts, _)) => facts,
            None => self
                .local_item(workspace, hash.as_str())
                .map_err(storage)?
                .map(|(_, facts, _)| facts)
                .unwrap_or_default(),
        };
        if let Some(refused) = facts.refusal(kind) {
            return Err(refused);
        }
        {
            let mut mirror = self
                .mirror
                .lock()
                .map_err(|_| storage("the readings mirror's lock is poisoned".to_owned()))?;
            // The attempt goes first: a coordinator that recomputed between
            // the two writes would otherwise see a request for an item that
            // failed for good, and drop it as met.
            if kind == koru_store_wire::readings::ReadRequestKind::ForceOcr
                || (kind == koru_store_wire::readings::ReadRequestKind::ReadAgain
                    && facts.failed_here)
            {
                mirror
                    .clear_attempts(hash.as_str())
                    .map_err(|e| storage(e.0))?;
            }
            mirror
                .request_with_ocr(
                    workspace,
                    hash.as_str(),
                    koru_store_core::model::Timestamp::now().millis(),
                    kind == koru_store_wire::readings::ReadRequestKind::ForceOcr,
                )
                .map_err(|e| storage(e.0))?;
        }
        self.inbox.read_requested(workspace, hash.as_str());
        Ok(koru_store_wire::readings::ReadRequested {
            workspace,
            hash: hash.as_str().to_owned(),
            kind,
        })
    }

    /// The Settings row's two lists, each item with the offers a menu on it
    /// would get (OCR Plan 3.5): the same [`Facts`], gathered for the item's
    /// own entry from the read models — its mirror row, the attempts table,
    /// the requests — and, only for the items listed, the Workspace's blob
    /// rows (one Store lock per workspace) and the disk.
    fn items(&self) -> Result<koru_store_wire::reader::ReaderItems, String> {
        let reader_ready = self.reader_ready();
        let mut rows: std::collections::HashMap<
            WorkspaceId,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
        let mut place = |workspace: WorkspaceId, hash: &str| -> (Option<String>, bool) {
            let blobs = rows.entry(workspace).or_insert_with(|| {
                self.state
                    .upgrade()
                    .and_then(|state| {
                        let mut store = state.store.lock().ok()?;
                        store.blobs(workspace).ok()
                    })
                    .map(|blobs| {
                        blobs
                            .into_iter()
                            .map(|b| (b.hash.as_str().to_owned(), b.mime))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            let mime = blobs.get(hash).cloned();
            let here = mime.is_some()
                && self
                    .objects
                    .as_ref()
                    .is_some_and(|objects| objects.here(hash));
            (mime, here)
        };
        self.items.items(
            koru_store_core::model::Timestamp::now().millis(),
            reader_ready,
            &mut place,
        )
    }
}

/// The answer for a live item without a reading, with an optional local failure.
fn local_answer(
    workspace: WorkspaceId,
    hash: &str,
    document: koru_store_core::ids::DocumentId,
    code: Option<String>,
    offers: koru_store_wire::readings::ReadingOffers,
) -> koru_store_wire::readings::ReadingAnswer {
    koru_store_wire::readings::ReadingAnswer {
        corrections: Default::default(),
        workspace,
        hash: hash.to_owned(),
        document,
        reader: None,
        generation: koru_read::GENERATION,
        status: None,
        reason: None,
        source: None,
        confidence: None,
        pages_done: None,
        pages: Vec::new(),
        offers,
        failed_here: code,
        local: None,
    }
}

/// **What decides whether a request is offered and accepted** (OCR Plan 3.2),
/// gathered once per question. Pure from here on, so every case is tested.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Facts {
    status: Option<String>,
    reason: Option<String>,
    source: Option<String>,
    /// The reader's token, when the entry itself could be read.
    reader: Option<String>,
    /// The Workspace's blob row's media type; `None` when it has no row.
    mime: Option<String>,
    /// The Workspace may read the bytes **and** they are on this computer.
    here: bool,
    /// A reader is installed **and on**, and nothing says this computer
    /// cannot use it. Off reads as not installed (OCR Plan 3.5).
    reader_ready: bool,
    /// A request for this item is already waiting.
    pending: bool,
    /// **This computer gave up reading it** (OCR Plan 3.5): the local
    /// attempts table's verdict is *failed* — a format it cannot open, a file
    /// that crashed the reader twice, the memory limit. Never synced, so it
    /// may be all there is to go by.
    failed_here: bool,
}

impl Facts {
    fn local(&self) -> koru_store_wire::readings::ReadingLocal {
        koru_store_wire::readings::ReadingLocal {
            available_here: self.here,
            pending: self.pending,
            supported: self
                .mime
                .as_deref()
                .is_some_and(|mime| coordinator::readable(mime).is_some()),
            needs_reader: self.picture()
                || (self.status.as_deref() == Some("partial") && self.reason.is_none()),
            reader_ready: self.reader_ready,
        }
    }
    fn recognized_some(&self) -> bool {
        matches!(self.source.as_deref(), Some("recognized" | "mixed"))
            || self
                .reader
                .as_deref()
                .is_some_and(|r| r != koru_read::READER_TEXT)
    }

    fn picture(&self) -> bool {
        self.mime
            .as_deref()
            .is_some_and(|m| coordinator::readable(m) == Some(coordinator::Readable::Picture))
    }

    /// *Read all pages*: a PDF stopped at a page limit. **One stopped at the
    /// text-layer cap whose words all came from text layers needs no reader**
    /// (TECHNICAL §18): the worker finishes it alone.
    fn read_all_needs_reader(&self) -> bool {
        self.recognized_some()
    }

    /// *Read again*: a picture always needs the reader; a reading made with
    /// it does too.
    fn read_again_needs_reader(&self) -> bool {
        self.picture() || self.recognized_some()
    }

    fn refusal(
        &self,
        kind: koru_store_wire::readings::ReadRequestKind,
    ) -> Option<koru_store_core::errors::Error> {
        use koru_store_core::errors::{Code, Error};
        use koru_store_wire::readings::ReadRequestKind;
        let status = self.status.as_deref();
        let applies = match kind {
            ReadRequestKind::ReadAll => {
                status == Some("partial")
                    && self.reason.as_deref() == Some("page-cap")
                    && self.mime.as_deref().is_some_and(coordinator::is_pdf)
            }
            // *Read again* is shown once an item has been read or has failed
            // (EXPERIENCE §7) — on this computer only, too.
            ReadRequestKind::ReadAgain => {
                matches!(status, Some("read" | "empty" | "failed")) || self.failed_here
            }
            ReadRequestKind::ForceOcr => {
                self.picture() || self.mime.as_deref().is_some_and(coordinator::is_pdf)
            }
        };
        if !applies {
            return Some(Error::new(
                Code::InvalidRequest,
                match kind {
                    ReadRequestKind::ReadAll => "this item did not stop at a page limit",
                    ReadRequestKind::ReadAgain => "this item has not been read and has not failed",
                    ReadRequestKind::ForceOcr => {
                        "force OCR is available for supported pictures and PDFs"
                    }
                },
            ));
        }
        if self.pending {
            return Some(Error::new(
                Code::InvalidRequest,
                "a request for this item is already waiting",
            ));
        }
        if !self.here {
            return Some(Error::new(
                Code::BlobNotHere,
                "this item's bytes are not on this computer",
            ));
        }
        let needs_reader = match kind {
            ReadRequestKind::ReadAll => self.read_all_needs_reader(),
            ReadRequestKind::ReadAgain => self.read_again_needs_reader(),
            ReadRequestKind::ForceOcr => true,
        };
        if needs_reader && !self.reader_ready {
            return Some(Error::new(
                Code::ReaderUnavailable,
                "this needs the reader, and it is not installed here",
            ));
        }
        None
    }

    fn offers(&self) -> koru_store_wire::readings::ReadingOffers {
        use koru_store_wire::readings::{ReadRequestKind, ReadingOffers};
        ReadingOffers {
            read_all: self.refusal(ReadRequestKind::ReadAll).is_none(),
            read_again: self.refusal(ReadRequestKind::ReadAgain).is_none(),
            force_ocr: self.refusal(ReadRequestKind::ForceOcr).is_none(),
        }
    }
}

/// The answer, from the reading document's entry when there is one and from
/// the mirror's row and words otherwise. Pure, so both shapes are tested.
fn answer(
    workspace: WorkspaceId,
    parent: koru_store_core::ids::DocumentId,
    row: &mirror::ReadingRow,
    entry: Option<koru_read::Reading>,
    words: Vec<(i64, String)>,
) -> koru_store_wire::readings::ReadingAnswer {
    use koru_store_wire::readings::{ReadingAnswer, ReadingPage};
    match entry {
        Some(reading) => ReadingAnswer {
            corrections: corrections::wire(&reading.corrections),
            workspace,
            hash: row.hash.clone(),
            document: parent,
            reader: reading.reader.map(|r| r.as_str().to_owned()),
            generation: reading.generation,
            status: reading.status.map(|s| s.as_str().to_owned()),
            reason: reading.reason.map(|r| r.as_str().to_owned()),
            source: reading.source.map(|s| s.as_str().to_owned()),
            confidence: reading.confidence,
            pages_done: reading.pages_done,
            pages: reading
                .pages
                .into_iter()
                .map(|(page, text)| ReadingPage {
                    page,
                    text: text.into_string(),
                })
                .collect(),
            offers: Default::default(),
            failed_here: None,
            local: None,
        },
        None => ReadingAnswer {
            corrections: Default::default(),
            workspace,
            hash: row.hash.clone(),
            document: parent,
            reader: None,
            generation: u32::try_from(row.generation).unwrap_or(0),
            status: row.status.clone(),
            reason: row.reason.clone(),
            source: row.source.clone(),
            confidence: None,
            pages_done: None,
            pages: words
                .into_iter()
                .filter_map(|(page, text)| {
                    Some(ReadingPage {
                        page: u32::try_from(page).ok()?,
                        text,
                    })
                })
                .collect(),
            offers: Default::default(),
            failed_here: None,
            local: None,
        },
    }
}

/// **The index's source of words**, answered from the mirror.
struct MirrorReadings(Mutex<mirror::Mirror>);

impl koru_search::Readings for MirrorReadings {
    fn readings(
        &self,
        workspace: WorkspaceId,
        hashes: &[String],
    ) -> Option<Vec<koru_search::ReadingText>> {
        // `None` only when the mirror genuinely cannot answer: a poisoned lock
        // or a read that failed. A hash with no words is simply absent.
        let mirror = self.0.lock().ok()?;
        match mirror.readings_for(workspace, hashes) {
            Ok(found) => Some(found),
            Err(error) => {
                tracing::debug!(target: "koru_readings", "the readings mirror could not answer: {error}");
                None
            }
        }
    }
}

/// What both threads publish, merged into `AppState::readings` for
/// `GET /v1/debug/readings`. **Ids, counts and stages only.**
struct Board {
    state: Weak<AppState>,
    coordinator: Mutex<serde_json::Value>,
    mirror: Mutex<serde_json::Value>,
    current: Mutex<Option<coordinator::Current>>,
}

impl Board {
    fn new(state: &Arc<AppState>) -> Self {
        Self {
            state: Arc::downgrade(state),
            coordinator: Mutex::new(serde_json::Value::Null),
            mirror: Mutex::new(serde_json::Value::Null),
            current: Mutex::new(None),
        }
    }

    fn set_current(&self, current: coordinator::Current) {
        if let Ok(mut slot) = self.current.lock() {
            *slot = Some(current);
        }
    }

    fn coordinator(&self, value: serde_json::Value) {
        if let Ok(mut slot) = self.coordinator.lock() {
            *slot = value;
        }
        self.publish();
    }

    fn mirror(&self, value: serde_json::Value) {
        if let Ok(mut slot) = self.mirror.lock() {
            *slot = value;
        }
        self.publish();
    }

    fn publish(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut coordinator = self
            .coordinator
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        let job = self
            .current
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .and_then(|current| current.lock().ok().and_then(|job| job.clone()));
        if let serde_json::Value::Object(map) = &mut coordinator {
            map.insert("job".into(), serde_json::to_value(job).unwrap_or_default());
        }
        let mirror = self.mirror.lock().map(|v| v.clone()).unwrap_or_default();
        if let Ok(mut slot) = state.readings.lock() {
            *slot = serde_json::json!({
                "schema_version": mirror::READINGS_SCHEMA_VERSION,
                "generation": koru_read::GENERATION,
                "coordinator": coordinator,
                "mirror": mirror,
            });
        }
    }
}

fn coordinator_report(coordinator: &coordinator::Coordinator) -> serde_json::Value {
    serde_json::json!({
        "running": true,
        "queued": coordinator.counts,
        "done": coordinator.stats,
        "worker": coordinator.launcher_report(),
        "waiting_because": coordinator.worker_down.as_ref().map(|why| why.code()),
        "paused_because": coordinator.paused.then_some("battery-saver"),
    })
}

fn mirror_report(applier: &applier::Applier, inbox: &Inbox) -> serde_json::Value {
    let (documents, workspaces) = inbox.mirror_backlog();
    let counts = applier.mirror().counts().unwrap_or_default();
    serde_json::json!({
        "behind": { "documents": documents, "workspaces": workspaces },
        "applied": applier.stats,
        "readings_by_status": counts.by_status,
        "failed_by_reason": counts.failed_by_reason,
        "page_capped": counts.page_capped,
        "local_failures_by_code": counts.attempts_by_code,
        "reading_documents": counts.documents,
        "pages": counts.pages,
    })
}

/// `koru-store-server readings [<hash>]` — look at the mirror.
///
/// **Reads `readings.db` directly**, like `projection` and `search`, so it
/// answers when the worker or the daemon will not.
///
/// **What it prints.** By default: counts, and one line per reading — ids, a
/// hash, a generation, status tokens and a page count; never a word. With a
/// hash: that hash's rows, and **its words, page by page** — asking for one
/// file's text is the point of naming it, and the person asking already holds
/// the file. Nothing prints a file's name or a title: the mirror holds none.
pub fn report_to_console(root: &Path, hash: Option<&str>) -> koru_store_core::errors::Result<()> {
    use koru_store_core::errors::{Code, Error};

    let path = root.join("readings.db");
    if !path.exists() {
        println!("No readings mirror at {}.", path.display());
        println!("Nothing has been read on this store root yet, or the mirror was deleted —");
        println!(
            "either way it is rebuilt from the reading documents the next time the store runs."
        );
        return Ok(());
    }
    let failed = |e: mirror::MirrorError| Error::new(Code::StorageFailed, format!("readings: {e}"));
    let mirror = mirror::Mirror::open(root).map_err(failed)?;

    let Some(hash) = hash else {
        let counts = mirror.counts().map_err(failed)?;
        println!("{}", path.display());
        println!(
            "{} reading document(s), {} page(s) of words",
            counts.documents, counts.pages
        );
        for (status, n) in &counts.by_status {
            println!("  {status:<10} {n}");
        }
        for (reason, n) in &counts.failed_by_reason {
            println!("  failed/{reason:<10} {n}");
        }
        if counts.page_capped > 0 {
            println!("  partial/page-cap {}", counts.page_capped);
        }
        for (code, n) in &counts.attempts_by_code {
            println!("  local {code:<12} {n}");
        }
        println!();
        for row in mirror.rows_for(None).map_err(failed)? {
            println!(
                "  {}  ws {}  doc {}  gen {}  {}{}  {} page(s)",
                row.hash,
                row.workspace,
                row.parent,
                row.generation,
                row.status.as_deref().unwrap_or("unknown"),
                row.reason.map(|r| format!("/{r}")).unwrap_or_default(),
                row.page_count
            );
        }
        println!();
        println!("A hash prints that reading, with its words.");
        return Ok(());
    };

    let rows = mirror.rows_for(Some(hash)).map_err(failed)?;
    if rows.is_empty() {
        println!("{hash} has no reading in this mirror.");
        return Ok(());
    }
    let mut shown = std::collections::BTreeSet::new();
    for row in &rows {
        println!(
            "workspace {}  document {}  generation {}  {}{}  source {}  {} page(s)",
            row.workspace,
            row.parent,
            row.generation,
            row.status.as_deref().unwrap_or("unknown"),
            row.reason
                .as_deref()
                .map(|r| format!("/{r}"))
                .unwrap_or_default(),
            row.source.as_deref().unwrap_or("-"),
            row.page_count
        );
        if shown.insert(row.workspace.clone()) {
            for (page, text) in mirror.pages_of(&row.workspace, hash).map_err(failed)? {
                println!("--- Page {page} ---");
                println!("{text}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
