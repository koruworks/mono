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
pub(crate) mod feed;
pub(crate) mod launcher;
pub(crate) mod mirror;
pub(crate) mod power;
pub(crate) mod supervised;
// The reader that finds words in pictures: its installer and the state the
// installer, the coordinator, search and the routes share (OCR Plan 2.4).
pub(crate) mod hub;
pub(crate) mod installer;

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

        // The reader's installer, before the coordinator, so the model it
        // finds on the disk is what the first worker is launched with.
        let service: Arc<dyn koru_store_api::routes::reader::ReaderService> = match installer {
            Some(config) if launcher.is_some() && objects.is_some() => {
                let (service, handle) = installer::start(
                    config,
                    Arc::clone(&self.hub),
                    Arc::clone(&self.inbox),
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
        let mut coordinator = coordinator::Coordinator::new(
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
    let handle = std::thread::Builder::new()
        .name("koru-readings".into())
        .spawn(move || {
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
}

impl koru_store_api::routes::readings::ReadingService for ReadingPort {
    fn reading(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Result<Option<koru_store_wire::readings::ReadingAnswer>, String> {
        let hash = hash.as_str();
        let (row, words) = {
            let mirror = self
                .mirror
                .lock()
                .map_err(|_| "the readings mirror's lock is poisoned".to_owned())?;
            let Some(row) = mirror.winner_in(workspace, hash).map_err(|e| e.0)? else {
                return Ok(None);
            };
            let words = mirror.pages_of(&row.workspace, hash).map_err(|e| e.0)?;
            (row, words)
        };
        let parent = koru_store_core::ids::DocumentId::parse(&row.parent)
            .ok_or_else(|| "the readings mirror holds a malformed document id".to_owned())?;
        let fork = self.state.upgrade().and_then(|state| {
            let mut store = state.store.lock().ok()?;
            store
                .fork_document(workspace, koru_store_core::ids::reading_document_id(parent))
                .ok()
        });
        let entry = fork.and_then(|fork| koru_read::ReadingDoc::from_doc(fork).reading(hash));
        Ok(Some(answer(workspace, parent, &row, entry, words)))
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
        },
        None => ReadingAnswer {
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
