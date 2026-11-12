// SPDX-License-Identifier: MPL-2.0

//! **The plan's verification, through the daemon's own pieces held in
//! memory** (OCR ROADMAP Plan 1.5): an in-memory Store opened as
//! `daemon::run` opens it, the real `StoreSource` with its readings tee, the
//! projector, the readings mirror and its applier, the coordinator, and the
//! search indexer with the mirror as its `Readings` — every database a named
//! database in memory, every worker a [`StubLauncher`], every "bytes on this
//! device" a set. No disk, no process, no network.
//!
//! Each device is driven by hand with a fake clock, the way
//! `not_projected_tests.rs` drives the projector.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Node, NodeId};
use koru_projection::Projector;
use koru_projection::worker::QUIET_MILLIS;
use koru_read::{
    Event, FailureCode, GENERATION, PageText, ReaderId, Reading, ReadingDoc, ReadingStatus, Reason,
    Request, Source,
};
use koru_store_api::state::{AppState, KoruStore};
use koru_store_core::ids::{DocumentId, WorkspaceId, reading_document_id};
use koru_store_core::model::{BlobHash, Timestamp};
use koru_store_core::ports::{CompletedBlobUpload, UpdateOrigin};
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};

use super::coordinator::{Coordinator, Objects, Pair};
use super::launcher::{Availability, Interrupted, StubLauncher, Unavailable};
use super::*;
use crate::derived::MemoryDerived;
use crate::projection::StoreSource;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + ms)
}

/// A well-formed hash, distinct per seed.
fn hash(seed: u8) -> String {
    format!("b3:{}", format!("{seed:02x}").repeat(32))
}

fn store() -> KoruStore {
    let catalog =
        SqliteCatalog::in_named_memory(&koru_sqlite::MemoryName::fresh("catalog.db")).unwrap();
    Store::open(catalog, SqliteBackend::in_memory())
        .unwrap()
        .with_unversioned_kinds(koru_projection::model::UNVERSIONED)
}

fn workspace(store: &mut KoruStore) -> WorkspaceId {
    let profile = store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap();
    store
        .create_workspace(profile.id, "Workspace", at(0))
        .unwrap()
        .id
}

/// A file entry whose root node carries `hash`, as the file importer makes one.
fn a_file(
    store: &mut KoruStore,
    ws: WorkspaceId,
    title: &str,
    hash: &str,
    mime: &str,
) -> DocumentId {
    let id = DocumentId::new();
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), title).unwrap();
    let mut root = Node::file(title, koru_core::BlobHash(hash.into()), mime).unwrap();
    root.id = NodeId::from_uuid(id.as_uuid());
    doc.insert(Parent::Root, root).unwrap();
    store
        .import_document_at_with_mime(
            ws,
            id,
            title,
            "file",
            Some(mime),
            None,
            None,
            doc.export_snapshot().unwrap(),
            at(1),
        )
        .unwrap()
        .0
}

/// A page holding one picture.
fn a_page_with_picture(store: &mut KoruStore, ws: WorkspaceId, hash: &str) -> DocumentId {
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Page").unwrap();
    let root = doc
        .insert(Parent::Root, Node::document("Page").unwrap())
        .unwrap();
    doc.insert(
        Parent::Node(root),
        Node::image(
            koru_core::BlobHash(hash.into()),
            "image/png",
            Some(10),
            Some(10),
        )
        .unwrap(),
    )
    .unwrap();
    store
        .import_document(
            ws,
            "Page",
            "page",
            None,
            doc.export_snapshot().unwrap(),
            at(1),
        )
        .unwrap()
        .0
}

/// The Workspace's `blobs` row: permission to read the bytes.
fn land(store: &mut KoruStore, ws: WorkspaceId, hash: &str, size: u64, mime: &str) {
    store
        .finish_blob_upload(
            ws,
            CompletedBlobUpload {
                hash: BlobHash::parse(hash).unwrap(),
                size,
            },
            mime,
            None,
            at(1),
            None,
        )
        .unwrap();
}

/// What the stub worker answers for one file.
#[derive(Clone)]
enum Script {
    Pages(Vec<(u32, &'static str)>, ReadingStatus, Option<Reason>),
    /// Dies after delivering these pages.
    Crash(Vec<(u32, &'static str)>),
    Fail(FailureCode),
}

type Library = Arc<Mutex<HashMap<String, Script>>>;

fn object_path(hash: &str) -> PathBuf {
    PathBuf::from(format!("objects/{}", hash.replace(':', "-")))
}

fn stub(library: Library) -> StubLauncher {
    StubLauncher::new(move |request: &Request| {
        let Request::Read {
            job,
            path,
            from_page,
            ..
        } = request
        else {
            return Ok(Vec::new());
        };
        let script = library
            .lock()
            .unwrap()
            .iter()
            .find(|(hash, _)| object_path(hash) == *path)
            .map(|(_, script)| script.clone())
            .expect("a scripted file");
        let page = |(n, text): &(u32, &str)| Event::Page {
            job: *job,
            page: *n,
            text: PageText::new(*text),
            source: Source::TextLayer,
            confidence: None,
        };
        match script {
            Script::Pages(pages, status, reason) => {
                let mut events: Vec<Event> = pages
                    .iter()
                    .filter(|(n, _)| n >= from_page)
                    .map(page)
                    .collect();
                events.push(Event::Done {
                    job: *job,
                    status,
                    reason,
                    pages_done: pages.iter().map(|(n, _)| *n).max().unwrap_or(0),
                });
                Ok(events)
            }
            Script::Crash(pages) => {
                let _ = pages.iter().filter(|(n, _)| n >= from_page).count();
                Err(Interrupted::Crashed {
                    memory_limit: false,
                })
            }
            Script::Fail(code) => Ok(vec![Event::Failed { job: *job, code }]),
        }
    })
}

struct Present(Arc<Mutex<HashSet<String>>>);

impl Objects for Present {
    fn here(&self, hash: &str) -> bool {
        self.0.lock().unwrap().contains(hash)
    }

    fn path(&self, hash: &str) -> Option<PathBuf> {
        Some(object_path(hash))
    }
}

/// One device: a Store and every derived piece beside it, in memory.
struct Device {
    state: Arc<AppState>,
    projector: Projector<StoreSource>,
    applier: applier::Applier,
    coordinator: Coordinator,
    indexer: koru_search::Indexer,
    reader: koru_search::SearchIndex,
    derived: Derived,
    queue: koru_search::Queue,
    present: Arc<Mutex<HashSet<String>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    now: i64,
}

impl Device {
    fn new(store: KoruStore, launcher: StubLauncher) -> Device {
        let state = Arc::new(AppState::new(store, "memory".into(), "test".into(), 0));
        let derived = Derived::Memory(Arc::new(MemoryDerived::open().unwrap()));
        let prepared = prepare(&derived).expect("the mirror opens in memory");
        let (writer, reader) = derived.search_pair().unwrap();
        let queue = koru_search::Queue::default();
        let mut projector = Projector::new(
            StoreSource::new(Arc::clone(&state), false).with_readings(Some(prepared.tee())),
            derived.projections().unwrap(),
        );
        projector.add_consumer(Box::new(koru_search::Feed::new(queue.clone())));
        projector.add_consumer(prepared.consumer());
        let indexer =
            koru_search::Indexer::new(writer, derived.projections().unwrap(), queue.clone())
                .with_readings(prepared.source().unwrap());
        let inbox = prepared.inbox();
        let applier = applier::Applier::new(
            &state,
            derived.mirror().unwrap(),
            derived.projections().ok(),
            Arc::clone(&inbox),
            Some(queue.clone()),
        );
        let present = Arc::new(Mutex::new(HashSet::new()));
        let requests = Arc::clone(&launcher.requests);
        let coordinator = Coordinator::new(
            &state,
            derived.projections().unwrap(),
            derived.mirror().unwrap(),
            Box::new(launcher),
            Box::new(Present(Arc::clone(&present))),
            inbox,
        );
        Device {
            state,
            projector,
            applier,
            coordinator,
            indexer,
            reader,
            derived,
            queue,
            present,
            requests,
            now: 0,
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, KoruStore> {
        self.state.store.lock().unwrap()
    }

    fn here(&self, hash: &str) {
        self.present.lock().unwrap().insert(hash.to_owned());
    }

    /// Run every piece until none has anything left to do: the projector past
    /// its debounce, the mirror, the coordinator, the indexer past its
    /// reconcile.
    fn settle(&mut self) {
        let mut quiet = 0;
        for _ in 0..400 {
            let mut busy = false;
            while self.projector.tick(self.now) {
                busy = true;
            }
            busy |= self.applier.step();
            while self.coordinator.step(self.now) {
                busy = true;
                busy |= self.applier.step();
            }
            busy |= self.applier.step();
            while self.indexer.tick(self.now) {
                busy = true;
            }
            busy |= self.projector.stats().pending > 0 || !self.queue.is_empty();
            self.now += QUIET_MILLIS.max(koru_search::worker::RECONCILE_MILLIS) + 1;
            quiet = if busy { 0 } else { quiet + 1 };
            if quiet >= 3 {
                return;
            }
        }
        panic!("the device never settled");
    }

    fn reads(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The documents a search for `text` finds, with each hit's page.
    fn find(&self, text: &str) -> Vec<(String, Option<u32>, String)> {
        self.reader
            .query(
                &koru_search::SearchQuery {
                    text: text.to_owned(),
                    page_size: 50,
                    ..Default::default()
                },
                &[],
            )
            .unwrap()
            .hits
            .into_iter()
            .map(|hit| (hit.document.unwrap_or_default(), hit.page, hit.field))
            .collect()
    }

    fn found(&self, text: &str, document: DocumentId) -> bool {
        self.find(text)
            .iter()
            .any(|(found, _, field)| *found == document.to_storage() && field == "read")
    }

    fn reading(&self, ws: WorkspaceId, parent: DocumentId, hash: &str) -> Option<Reading> {
        let fork = self
            .store()
            .fork_document(ws, reading_document_id(parent))
            .ok()?;
        ReadingDoc::from_doc(fork).reading(hash)
    }
}

/// Everything `to` lacks of `document`, from `from`, applied and flushed —
/// one direction of a sync, as the replication path delivers it.
fn sync(from: &Device, to: &Device, ws: WorkspaceId, document: DocumentId) {
    let have = to.store().version(ws, document).unwrap_or_default();
    let update = from.store().updates_since(ws, document, &have).unwrap();
    let mut store = to.store();
    if store.manifest_id(ws).ok() != Some(document) {
        // The replication path provisions a document's storage on receipt.
        store
            .provision_replica_document(ws, document, at(5))
            .unwrap();
    }
    store
        .apply_update(ws, document, update, at(5), UpdateOrigin::Store)
        .unwrap();
    store.flush(ws, document, at(5)).unwrap();
}

fn library(entries: &[(&str, Script)]) -> Library {
    Arc::new(Mutex::new(
        entries
            .iter()
            .map(|(hash, script)| ((*hash).to_owned(), script.clone()))
            .collect(),
    ))
}

// ------------------------------------------------------------------ reading

/// **A qualifying blob is read once**, and its words find the file, page by
/// page — however many times the device is told about it again.
#[test]
fn a_qualifying_blob_is_read_once_and_its_words_find_the_file() {
    let pdf = hash(1);
    let library = library(&[(
        &pdf,
        Script::Pages(
            vec![
                (1, "the zanzibarquux contract"),
                (2, ""),
                (3, "signed on page three"),
            ],
            ReadingStatus::Read,
            None,
        ),
    )]);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Contract.pdf", &pdf, "application/pdf");
    land(&mut store, ws, &pdf, 2_000, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    device.settle();

    assert_eq!(device.reads(), 1, "read once");
    let reading = device.reading(ws, file, &pdf).expect("written");
    assert_eq!(reading.status, Some(ReadingStatus::Read));
    assert_eq!(reading.reader, Some(ReaderId::text()));
    assert_eq!(reading.generation, GENERATION);
    assert_eq!(reading.source, Some(Source::TextLayer));
    assert_eq!(reading.pages.len(), 2, "an empty page is not written");
    let hits = device.find("zanzibarquux");
    assert!(
        hits.iter()
            .any(|(d, page, field)| *d == file.to_storage() && *page == Some(1) && field == "read"),
        "found on page 1: {hits:?}"
    );
    assert!(device.found("three", file));

    // Told again — the bytes land again, the file is re-imported into the
    // projection, the listing moves — and nothing is read twice.
    land(&mut device.store(), ws, &pdf, 2_000, "application/pdf");
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(device.reads(), 1, "not read twice");
    assert_eq!(device.coordinator.counts.backlog, 0);
}

/// **A Trash-only blob is not queued**; it is counted, and restored it reads.
#[test]
fn a_trash_only_blob_is_not_queued() {
    let pdf = hash(2);
    let library = library(&[(
        &pdf,
        Script::Pages(vec![(1, "binned words")], ReadingStatus::Read, None),
    )]);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Old.pdf", &pdf, "application/pdf");
    land(&mut store, ws, &pdf, 10, "application/pdf");
    store.trash_document_entry(ws, file, at(2), None).unwrap();
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    device.settle();

    assert_eq!(device.reads(), 0, "a trashed entry is not read");
    assert_eq!(device.coordinator.counts.trashed, 1);
    assert!(device.coordinator.queued().is_empty());
}

/// **A blob whose bytes are absent is counted `not_here`** — and when the
/// bytes land, the second trigger reads it with nothing else happening.
#[test]
fn a_blob_whose_bytes_are_absent_is_counted_not_here_until_they_land() {
    let pdf = hash(3);
    let library = library(&[(
        &pdf,
        Script::Pages(
            vec![(1, "arrived later quokkaword")],
            ReadingStatus::Read,
            None,
        ),
    )]);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Late.pdf", &pdf, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.settle();
    assert_eq!(device.reads(), 0);
    assert_eq!(device.coordinator.counts.not_here, 1);

    // The bytes arrive: the Store hears it, and so does the coordinator.
    device.here(&pdf);
    land(&mut device.store(), ws, &pdf, 10, "application/pdf");
    device.settle();
    assert_eq!(device.reads(), 1);
    assert!(device.found("quokkaword", file));
}

/// **A restart recomputes the same queue**: smallest first, the same order
/// from the same state, with nothing persisted between.
#[test]
fn a_restart_recomputes_the_same_queue() {
    let mut store = store();
    let ws = workspace(&mut store);
    let mut expected = Vec::new();
    for (seed, size) in [(10_u8, 900_u64), (11, 50), (12, 400), (13, 50)] {
        let h = hash(seed);
        let file = a_file(&mut store, ws, "File.pdf", &h, "application/pdf");
        land(&mut store, ws, &h, size, "application/pdf");
        expected.push((size, file, h));
    }
    let mut unavailable = stub(library(&[]));
    unavailable.availability = Availability::Unavailable(Unavailable::NotInstalled);
    let mut device = Device::new(store, unavailable);
    for (_, _, h) in &expected {
        device.here(h);
    }
    device.settle();
    assert_eq!(device.reads(), 0, "no reader: nothing ran");
    assert_eq!(
        device.coordinator.worker_down,
        Some(Unavailable::NotInstalled)
    );

    device.coordinator.recompute(device.now);
    let first: Vec<Pair> = device.coordinator.queued();
    // A coordinator that has never run, over the same state.
    let mut again = Coordinator::new(
        &device.state,
        device.derived.projections().unwrap(),
        device.derived.mirror().unwrap(),
        Box::new(stub(library(&[]))),
        Box::new(Present(Arc::clone(&device.present))),
        Inbox::new(),
    );
    again.recompute(device.now);
    assert_eq!(again.queued(), first, "the same queue from the same state");
    let sizes: Vec<u64> = first.iter().map(|p| p.size).collect();
    assert_eq!(sizes, vec![50, 50, 400, 900], "smallest first");
    assert!(first.iter().all(|p| !p.fresh));
    assert_eq!(again.counts.backlog, 4);
}

/// **A freshly added file goes ahead of the backlog.**
#[test]
fn a_freshly_added_file_goes_ahead_of_the_backlog() {
    let mut store = store();
    let ws = workspace(&mut store);
    let backlog: Vec<String> = (20..23).map(hash).collect();
    for h in &backlog {
        a_file(&mut store, ws, "Old.pdf", h, "application/pdf");
        land(&mut store, ws, h, 10, "application/pdf");
    }
    let mut unavailable = stub(library(&[]));
    unavailable.availability = Availability::Unavailable(Unavailable::NotInstalled);
    let mut device = Device::new(store, unavailable);
    for h in &backlog {
        device.here(h);
    }
    device.settle();
    device.coordinator.recompute(device.now);

    let fresh = hash(29);
    device.here(&fresh);
    {
        let mut store = device.store();
        a_file(&mut store, ws, "New.pdf", &fresh, "application/pdf");
        land(&mut store, ws, &fresh, 5_000_000, "application/pdf");
    }
    // The projector hears of the new file; its trigger reaches the
    // coordinator, which puts the pair first despite its size.
    for _ in 0..5 {
        device.projector.tick(device.now);
        device.now += QUIET_MILLIS + 1;
    }
    device.coordinator.step(device.now);
    let queued = device.coordinator.queued();
    let first = device.coordinator.counts.waiting_for_reader;
    assert_eq!(first, 1, "the head of the queue waited for a reader");
    assert!(
        queued.iter().all(|p| p.hash != fresh),
        "the fresh pair was taken first and is waiting: {queued:?}"
    );
}

// ------------------------------------------------------------- the copy rule

/// **The same bytes in two entries are read once and written twice**, and a
/// purged parent's words leave the mirror only when no other entry in the
/// workspace still holds the same picture.
#[test]
fn the_same_bytes_are_read_once_and_a_purge_takes_words_only_with_the_last_holder() {
    let pdf = hash(4);
    let library = library(&[(
        &pdf,
        Script::Pages(vec![(1, "shared wombatword")], ReadingStatus::Read, None),
    )]);
    let mut store = store();
    let ws = workspace(&mut store);
    let first = a_file(&mut store, ws, "One.pdf", &pdf, "application/pdf");
    let second = a_file(&mut store, ws, "Two.pdf", &pdf, "application/pdf");
    land(&mut store, ws, &pdf, 10, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    device.settle();

    assert_eq!(device.reads(), 1, "read once");
    assert_eq!(device.coordinator.stats.copied, 1, "and copied once");
    assert!(device.reading(ws, first, &pdf).is_some());
    assert!(device.reading(ws, second, &pdf).is_some());
    assert!(device.found("wombatword", first));
    assert!(device.found("wombatword", second));

    let purge = |device: &Device, file: DocumentId| {
        let mut store = device.store();
        let manifest_id = store.manifest_id(ws).unwrap();
        let mut client = koru_store_core::manifest::Manifest::from_snapshot(
            &store.snapshot(ws, manifest_id).unwrap(),
        )
        .unwrap();
        let before = client.version();
        client.remove_document(file).unwrap();
        store
            .apply_update(
                ws,
                manifest_id,
                client.export_updates_since(&before).unwrap(),
                at(9),
                UpdateOrigin::Store,
            )
            .unwrap();
        store.flush(ws, manifest_id, at(9)).unwrap();
    };
    let words = |device: &Device| {
        device
            .applier
            .mirror()
            .readings_for(ws, std::slice::from_ref(&pdf))
            .unwrap()
    };

    purge(&device, first);
    device.settle();
    assert_eq!(words(&device).len(), 1, "the other entry still holds it");
    assert!(!device.found("wombatword", first));
    assert!(device.found("wombatword", second));

    purge(&device, second);
    device.settle();
    assert!(words(&device).is_empty(), "the last holder took the words");
    assert!(device.find("wombatword").is_empty());
}

// ------------------------------------------------- readings that arrive late

/// **A reading written after its parent was indexed makes the parent findable
/// by those words, with no edit to the parent.**
#[test]
fn a_reading_written_after_its_parent_was_indexed_makes_it_findable() {
    let pdf = hash(5);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Indexed.pdf", &pdf, "application/pdf");
    let mut device = Device::new(store, stub(library(&[])));
    device.settle();
    assert!(
        device
            .find("Indexed")
            .iter()
            .any(|(d, ..)| *d == file.to_storage())
    );
    assert!(device.find("narwhalword").is_empty());

    // Somebody writes the reading — not the parent.
    {
        let mut store = device.store();
        let reading = store.ensure_reading(ws, file, at(3)).unwrap().id;
        let doc = ReadingDoc::from_doc(store.fork_document(ws, reading).unwrap());
        let before = doc.version();
        doc.write(
            &pdf,
            &Reading {
                reader: Some(ReaderId::text()),
                generation: GENERATION,
                status: Some(ReadingStatus::Read),
                pages: [(1, PageText::new("late narwhalword"))].into(),
                ..Reading::default()
            },
        )
        .unwrap();
        store
            .apply_update(
                ws,
                reading,
                doc.export_updates_since(&before).unwrap(),
                at(4),
                UpdateOrigin::Store,
            )
            .unwrap();
        store.flush(ws, reading, at(4)).unwrap();
    }
    let parent_revision = device.store().version(ws, file).unwrap();
    device.settle();
    assert!(device.found("narwhalword", file));
    assert_eq!(
        device.store().version(ws, file).unwrap(),
        parent_revision,
        "the parent was not edited"
    );
}

/// **Two stores syncing converge on the same words**, and **a reading that
/// arrives from the other store makes the file findable** on a device that
/// never read it and does not hold its bytes.
#[test]
fn two_stores_converge_and_a_reading_from_the_other_store_is_found() {
    let pdf = hash(6);
    let words = Script::Pages(
        vec![(1, "first page"), (2, "the pelicanword clause")],
        ReadingStatus::Read,
        None,
    );
    let mut a_store = store();
    let ws = workspace(&mut a_store);
    let file = a_file(&mut a_store, ws, "Shared.pdf", &pdf, "application/pdf");
    land(&mut a_store, ws, &pdf, 10, "application/pdf");
    let record = a_store.workspace(ws).unwrap();
    let profile = a_store.profile(record.owner).unwrap();
    let manifest = a_store.manifest_id(ws).unwrap();
    let mut a = Device::new(a_store, stub(library(&[(&pdf, words.clone())])));
    a.here(&pdf);

    let mut b_store = store();
    b_store
        .provision_replica(&profile, &record, manifest, at(1))
        .unwrap();
    let mut b = Device::new(b_store, stub(library(&[(&pdf, words)])));
    sync(&a, &b, ws, manifest);
    sync(&a, &b, ws, file);

    // B has the file and not its bytes: nothing to read there.
    b.settle();
    assert_eq!(b.reads(), 0);
    assert_eq!(b.coordinator.counts.not_here, 1);
    assert!(!b.found("pelicanword", file));

    // A reads it; the reading document travels like any document.
    a.settle();
    assert_eq!(a.reads(), 1);
    let reading = reading_document_id(file);
    sync(&a, &b, ws, manifest);
    sync(&a, &b, ws, reading);
    b.settle();
    assert!(
        b.found("pelicanword", file),
        "found on B, which never read it"
    );
    assert_eq!(b.reads(), 0);
    let hits = b.find("pelicanword");
    assert!(hits.iter().any(|(_, page, _)| *page == Some(2)), "{hits:?}");
    assert_eq!(a.reading(ws, file, &pdf), b.reading(ws, file, &pdf));
}

/// **Two stores that each read the same file while apart converge on the
/// same words**: each registered the same reading document (its id is
/// derived) and wrote into it; after they meet, one document holds one
/// reading on both, and both find the file by it.
#[test]
fn two_stores_that_read_apart_converge_on_the_same_words() {
    let pdf = hash(18);
    let words = Script::Pages(
        vec![(1, "apart cormorantword"), (2, "second page")],
        ReadingStatus::Read,
        None,
    );
    let mut a_store = store();
    let ws = workspace(&mut a_store);
    let file = a_file(&mut a_store, ws, "Both.pdf", &pdf, "application/pdf");
    let record = a_store.workspace(ws).unwrap();
    let profile = a_store.profile(record.owner).unwrap();
    let manifest = a_store.manifest_id(ws).unwrap();
    let mut b_store = store();
    b_store
        .provision_replica(&profile, &record, manifest, at(1))
        .unwrap();
    let mut a = Device::new(a_store, stub(library(&[(&pdf, words.clone())])));
    let mut b = Device::new(b_store, stub(library(&[(&pdf, words)])));
    sync(&a, &b, ws, manifest);
    sync(&a, &b, ws, file);
    for device in [&a, &b] {
        device.here(&pdf);
        land(&mut device.store(), ws, &pdf, 10, "application/pdf");
    }

    a.settle();
    b.settle();
    assert_eq!((a.reads(), b.reads()), (1, 1), "each read it, apart");

    let reading = reading_document_id(file);
    for _ in 0..2 {
        sync(&a, &b, ws, manifest);
        sync(&b, &a, ws, manifest);
        sync(&a, &b, ws, reading);
        sync(&b, &a, ws, reading);
    }
    a.settle();
    b.settle();
    let on_a = a.reading(ws, file, &pdf).unwrap();
    assert_eq!(Some(&on_a), b.reading(ws, file, &pdf).as_ref());
    assert_eq!(on_a.status, Some(ReadingStatus::Read));
    assert_eq!(on_a.pages.len(), 2, "no page held twice");
    for device in [&a, &b] {
        let rows = device
            .store()
            .documents(ws)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.id == reading)
            .count();
        assert_eq!(rows, 1, "one reading document");
        assert!(device.found("cormorantword", file));
    }
    assert_eq!((a.reads(), b.reads()), (1, 1), "and neither read it again");
}

/// **A reading that arrived while this store was stopped** — committed when
/// no mirror heard it — is applied by the catch-up when the mirror starts.
#[test]
fn a_reading_that_arrived_while_the_mirror_was_stopped_is_caught_up() {
    let pdf = hash(7);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Missed.pdf", &pdf, "application/pdf");
    let reading = store.ensure_reading(ws, file, at(2)).unwrap().id;
    let mut device = Device::new(store, stub(library(&[])));
    device.settle();

    // The words arrive and the commit is drained by nobody who tells the
    // mirror — the tee missed it, as it does for a store that was stopped.
    {
        let mut store = device.store();
        let doc = ReadingDoc::from_doc(store.fork_document(ws, reading).unwrap());
        let before = doc.version();
        doc.write(
            &pdf,
            &Reading {
                reader: Some(ReaderId::text()),
                generation: GENERATION,
                status: Some(ReadingStatus::Read),
                pages: [(1, PageText::new("missed ibexword"))].into(),
                ..Reading::default()
            },
        )
        .unwrap();
        store
            .apply_update(
                ws,
                reading,
                doc.export_updates_since(&before).unwrap(),
                at(4),
                UpdateOrigin::Store,
            )
            .unwrap();
        store.flush(ws, reading, at(4)).unwrap();
        let _ = store.take_committed();
    }
    device.settle();
    assert!(!device.found("ibexword", file), "nobody told the mirror");

    // The mirror starts again: a fresh applier over the same mirror, owed
    // its start-up catch-up.
    device.applier = applier::Applier::new(
        &device.state,
        device.derived.mirror().unwrap(),
        device.derived.projections().ok(),
        Inbox::new(),
        Some(device.queue.clone()),
    );
    device.settle();
    assert!(device.found("ibexword", file));
}

// --------------------------------------------------------- what is not read

/// **A picture needs the reader**, which this plan does not run: counted
/// `needs_reader`, never sent. A scan that needs it for some pages is written
/// `partial` with no reason, counted `needs_reader`, and not fed again.
#[test]
fn pictures_and_scanned_pages_wait_as_needs_reader() {
    let picture = hash(8);
    let scan = hash(9);
    let library = library(&[(
        &scan,
        Script::Pages(
            vec![(1, "a text page"), (2, "")],
            ReadingStatus::Partial,
            None,
        ),
    )]);
    let mut store = store();
    let ws = workspace(&mut store);
    a_page_with_picture(&mut store, ws, &picture);
    let file = a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    land(&mut store, ws, &picture, 10, "image/png");
    land(&mut store, ws, &scan, 10, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&picture);
    device.here(&scan);
    device.settle();

    assert_eq!(device.reads(), 1, "only the PDF was sent");
    let reading = device.reading(ws, file, &scan).unwrap();
    assert_eq!(reading.status, Some(ReadingStatus::Partial));
    assert_eq!(reading.reason, None);
    assert!(device.found("text", file), "its text page is found");

    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(device.reads(), 1, "not fed again");
    assert_eq!(device.coordinator.counts.needs_reader, 2);
}

/// **A file that crashes the worker** is recorded locally, retried once in a
/// fresh worker resuming past the page it died on, then left — never a loop
/// that burns the restart budget. A password-protected PDF is synced as
/// `failed/password` and not retried.
#[test]
fn a_crashing_file_gets_one_retry_and_a_password_is_final() {
    let hostile = hash(14);
    let locked = hash(15);
    let library = library(&[
        (&hostile, Script::Crash(vec![])),
        (
            &locked,
            Script::Pages(vec![], ReadingStatus::Failed, Some(Reason::Password)),
        ),
    ]);
    let mut store = store();
    let ws = workspace(&mut store);
    let crashing = a_file(&mut store, ws, "Bad.pdf", &hostile, "application/pdf");
    let password = a_file(&mut store, ws, "Locked.pdf", &locked, "application/pdf");
    land(&mut store, ws, &hostile, 10, "application/pdf");
    land(&mut store, ws, &locked, 20, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&hostile);
    device.here(&locked);
    device.settle();
    for _ in 0..3 {
        device.coordinator.recompute(device.now);
        device.settle();
    }

    let requests = device.requests.lock().unwrap().clone();
    let crashed: Vec<u32> = requests
        .iter()
        .filter_map(|r| match r {
            Request::Read {
                path, from_page, ..
            } if *path == object_path(&hostile) => Some(*from_page),
            _ => None,
        })
        .collect();
    assert_eq!(crashed, vec![1, 2], "one retry, past the page it died on");
    assert_eq!(device.coordinator.counts.failed, 1);
    let partial = device.reading(ws, crashing, &hostile).unwrap();
    assert_eq!(partial.status, Some(ReadingStatus::Partial));

    let failed = device.reading(ws, password, &locked).unwrap();
    assert_eq!(failed.status, Some(ReadingStatus::Failed));
    assert_eq!(failed.reason, Some(Reason::Password));
    assert_eq!(
        requests
            .iter()
            .filter(|r| matches!(r, Request::Read { path, .. } if *path == object_path(&locked)))
            .count(),
        1,
        "a password is true everywhere and final"
    );
}

/// A format this computer cannot decode is a local fact: recorded, not
/// synced, not retried at this generation.
#[test]
fn an_unsupported_file_is_recorded_locally_and_not_synced() {
    let odd = hash(16);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Odd.pdf", &odd, "application/pdf");
    land(&mut store, ws, &odd, 10, "application/pdf");
    let mut device = Device::new(
        store,
        stub(library(&[(&odd, Script::Fail(FailureCode::Unsupported))])),
    );
    device.here(&odd);
    device.settle();
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(device.reads(), 1);
    assert!(
        device.reading(ws, file, &odd).is_none(),
        "nothing was written into the synced document"
    );
    assert_eq!(device.coordinator.counts.failed, 1);
}

// ------------------------------------------------------------ the debug board

/// The debug route's JSON carries ids, counts and stages — and no word read
/// and no title, asserted by searching it for the fixture's own strings.
#[test]
fn the_debug_board_carries_no_word_and_no_title() {
    let pdf = hash(17);
    let mut store = store();
    let ws = workspace(&mut store);
    a_file(
        &mut store,
        ws,
        "Secretive Title.pdf",
        &pdf,
        "application/pdf",
    );
    land(&mut store, ws, &pdf, 10, "application/pdf");
    let mut device = Device::new(
        store,
        stub(library(&[(
            &pdf,
            Script::Pages(vec![(1, "confidentialword")], ReadingStatus::Read, None),
        )])),
    );
    device.here(&pdf);
    device.settle();
    let board = Board::new(&device.state);
    board.coordinator(coordinator_report(&device.coordinator));
    board.mirror(mirror_report(&device.applier, &Inbox::default()));
    let published = device.state.readings.lock().unwrap().to_string();
    for forbidden in ["confidentialword", "Secretive", pdf.as_str()] {
        assert!(!published.contains(forbidden), "{forbidden} in {published}");
    }
    for expected in [
        "queued",
        "needs_reader",
        "not_here",
        "backlog",
        "worker",
        "behind",
    ] {
        assert!(
            published.contains(expected),
            "{expected} missing: {published}"
        );
    }
}

/// **One reading's answer** carries what the reading document says when it
/// can be read, and falls back to the mirror's row and words when it cannot,
/// with what only the document knows left absent.
#[test]
fn one_readings_answer_comes_from_the_document_and_falls_back_to_the_mirror() {
    let ws = WorkspaceId::new();
    let parent = DocumentId::new();
    let row = mirror::ReadingRow {
        workspace: ws.to_storage(),
        parent: parent.to_storage(),
        hash: "b3:aa".into(),
        generation: 1,
        status: Some("partial".into()),
        reason: Some("page-cap".into()),
        source: Some("recognized".into()),
        page_count: 2,
    };
    let words = vec![(1, "one".to_owned()), (2, "two".to_owned())];
    let entry = Reading {
        reader: Some(ReaderId::text()),
        generation: 1,
        status: Some(ReadingStatus::Partial),
        reason: Some(Reason::PageCap),
        source: Some(Source::Recognized),
        confidence: Some(87),
        pages_done: Some(2),
        pages: [(1, PageText::new("one")), (2, PageText::new("two"))].into(),
    };

    let full = answer(ws, parent, &row, Some(entry), words.clone());
    assert_eq!(full.reader.as_deref(), Some("text"));
    assert_eq!(full.confidence, Some(87));
    assert_eq!(full.pages_done, Some(2));
    assert_eq!(full.reason.as_deref(), Some("page-cap"));
    assert_eq!(full.document, parent);
    assert_eq!(
        full.pages
            .iter()
            .map(|p| (p.page, p.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "one"), (2, "two")]
    );

    let mirrored = answer(ws, parent, &row, None, words);
    assert_eq!(mirrored.reader, None);
    assert_eq!(mirrored.confidence, None);
    assert_eq!(mirrored.status.as_deref(), Some("partial"));
    assert_eq!(mirrored.source.as_deref(), Some("recognized"));
    assert_eq!(mirrored.generation, 1);
    assert_eq!(mirrored.pages, full.pages, "the same words either way");
}

/// **The worker is handed the object the blob store wrote**:
/// `<root>/blobs/objects/<algorithm>-<digest>`. Found by the first run against
/// a real root (OCR Plan 1.8): the path was `<root>/blobs/<…>`, one level
/// short, so every file counted `not_here` — and nothing in memory could say
/// so, because a stub never opens a path. Composition only; nothing is read.
#[test]
fn a_device_hands_the_worker_the_object_the_blob_store_wrote() {
    let root = PathBuf::from("store-root");
    let objects = objects_under(&root);
    assert_eq!(
        objects.path("b3:ab12"),
        Some(root.join("blobs").join("objects").join("b3-ab12"))
    );
    assert_eq!(objects.path("not a hash"), None);
}
