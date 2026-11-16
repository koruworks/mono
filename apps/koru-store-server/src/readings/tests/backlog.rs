// SPDX-License-Identifier: MPL-2.0

//! **The backlog** (OCR ROADMAP Plan 3.1; TECHNICAL §9 "Registering during a
//! backlog", §14 "Staying out of the way"): a thousand files are a handful of
//! manifest commits and are read smallest first, whether they were waiting at
//! start or arrive as an import; a second workspace holding the same bytes is
//! not read again, and a fuller reading reaches it; the backlog waits out
//! battery saver and a fresh file does not.
//!
//! Through the same in-memory device as the rest of the plan's verification:
//! no disk, no process, no machine power state.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// A thousand: the plan's number.
const FILES: u32 = 1_000;

/// A stub worker that reads any file as one page of words, so a thousand
/// files need no thousand scripts.
fn any_file() -> StubLauncher {
    StubLauncher::new(|request: &Request| {
        let Request::Read { job, .. } = request else {
            return Ok(Vec::new());
        };
        Ok(vec![
            Event::Page {
                job: *job,
                page: 1,
                text: PageText::new("synthetic words"),
                source: Source::TextLayer,
                confidence: None,
            },
            Event::Done {
                job: *job,
                status: ReadingStatus::Read,
                reason: None,
                pages_done: 1,
            },
        ])
    })
}

/// A well-formed hash for file `n`.
fn nth(n: u32) -> String {
    format!("b3:{:064x}", u64::from(n) + 0x1000)
}

/// Sizes that are not in any order, from a fixed seed: a few bytes to fifty
/// megabytes, as an archive's PDFs and pictures are.
fn sizes(count: u32) -> Vec<u64> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            1 + state % 50_000_000
        })
        .collect()
}

/// Import `count` files into `ws`, each with its blob row, and return each
/// object's path with its size — what the stub is asked to open, and how big
/// it is.
fn import(
    store: &mut KoruStore,
    ws: WorkspaceId,
    count: u32,
    first: u32,
) -> (Vec<String>, HashMap<PathBuf, u64>) {
    let mut hashes = Vec::new();
    let mut by_path = HashMap::new();
    for (i, size) in sizes(count).into_iter().enumerate() {
        let h = nth(first + i as u32);
        a_file(store, ws, "File.pdf", &h, "application/pdf");
        land(store, ws, &h, size, "application/pdf");
        by_path.insert(object_path(&h), size);
        hashes.push(h);
    }
    (hashes, by_path)
}

/// Run the projector until it is quiet, so every file is in the projection
/// and its trigger is in the coordinator's inbox.
fn project(device: &mut Device) {
    for _ in 0..8 {
        while device.projector.tick(device.now) {}
        device.now += QUIET_MILLIS + 1;
    }
}

/// **Drive the coordinator alone and count the manifest's commits from the
/// Store itself.** Each step's commits are drained as it ends; a manifest
/// commit carries its journal position, so the positions seen must run
/// without a gap — a commit this count missed would leave one.
fn read_counting_manifest_commits(device: &mut Device) -> Vec<u64> {
    // What the import committed is not the coordinator's.
    device.store().take_committed();
    let mut positions = Vec::new();
    let mut steps = 0;
    while device.coordinator.step(device.now) {
        steps += 1;
        assert!(
            steps < 10 * FILES as usize,
            "the coordinator never finished"
        );
        positions.extend(
            device
                .store()
                .take_committed()
                .into_iter()
                .filter(|committed| committed.is_manifest)
                .map(|committed| committed.seq),
        );
    }
    positions
}

fn assert_a_handful(positions: &[u64], device: &Device) {
    let expected = FILES.div_ceil(coordinator::REGISTER_BATCH as u32) as usize;
    println!(
        "{FILES} files: {} manifest commits (journal positions {:?} to {:?}); \
         coordinator says {} commits, {} registered",
        positions.len(),
        positions.first(),
        positions.last(),
        device.coordinator.stats.manifest_commits,
        device.coordinator.stats.registered
    );
    assert_eq!(positions.len(), expected, "a handful, not {FILES}");
    assert!(
        positions.windows(2).all(|w| w[1] == w[0] + 1),
        "a manifest commit went uncounted"
    );
    assert_eq!(
        device.coordinator.stats.manifest_commits,
        positions.len() as u64
    );
    assert_eq!(device.coordinator.stats.registered, u64::from(FILES));
}

/// The sizes of the files the worker was asked to read, in the order asked.
fn read_order(device: &Device, by_path: &HashMap<PathBuf, u64>) -> Vec<u64> {
    device
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| match request {
            Request::Read { path, .. } => by_path.get(path).copied(),
            _ => None,
        })
        .collect()
}

/// **A thousand files waiting at start are five manifest commits, and are
/// read smallest first.**
#[test]
fn a_thousand_waiting_files_are_a_handful_of_manifest_commits_read_smallest_first() {
    let mut store = store();
    let ws = workspace(&mut store);
    let (hashes, by_path) = import(&mut store, ws, FILES, 0);
    let mut device = Device::new(store, any_file());
    for h in &hashes {
        device.here(h);
    }
    project(&mut device);

    let positions = read_counting_manifest_commits(&mut device);
    assert_eq!(device.reads(), FILES as usize, "each file read once");
    assert_a_handful(&positions, &device);
    let order = read_order(&device, &by_path);
    assert_eq!(order.len(), FILES as usize);
    assert!(
        order.windows(2).all(|w| w[0] <= w[1]),
        "not smallest first: {:?}…",
        &order[..20]
    );
}

/// **A thousand files arriving as an import, while the coordinator runs, are
/// five manifest commits too** — they reach the fresh lane together, and a
/// batch takes fresh pairs as it takes the backlog — and the fresh lane is
/// smallest first among itself.
#[test]
fn a_thousand_imported_files_are_a_handful_of_manifest_commits_too() {
    let mut store = store();
    let ws = workspace(&mut store);
    let mut device = Device::new(store, any_file());
    device.settle();
    assert_eq!(device.coordinator.stats.manifest_commits, 0);

    let (hashes, by_path) = {
        let mut store = device.store();
        import(&mut store, ws, FILES, 0)
    };
    for h in &hashes {
        device.here(h);
    }
    project(&mut device);

    let positions = read_counting_manifest_commits(&mut device);
    assert_eq!(device.reads(), FILES as usize);
    assert_a_handful(&positions, &device);
    let order = read_order(&device, &by_path);
    assert!(order.windows(2).all(|w| w[0] <= w[1]), "not smallest first");
}

/// **A file added on its own registers at once, alone**: no batch to wait for.
#[test]
fn a_file_on_its_own_registers_and_is_read_at_once() {
    let mut store = store();
    let ws = workspace(&mut store);
    let mut device = Device::new(store, any_file());
    device.settle();
    let h = nth(1);
    device.here(&h);
    {
        let mut store = device.store();
        a_file(&mut store, ws, "One.pdf", &h, "application/pdf");
        land(&mut store, ws, &h, 10, "application/pdf");
    }
    project(&mut device);
    let positions = read_counting_manifest_commits(&mut device);
    assert_eq!(positions.len(), 1);
    assert_eq!(device.reads(), 1);
}

// ------------------------------------------------------ the copy across workspaces

fn second_workspace(store: &mut KoruStore) -> WorkspaceId {
    let profile = store
        .create_profile("Other", None, None, Locale::parse("en").unwrap(), at(0))
        .unwrap();
    store
        .create_workspace(profile.id, "Other workspace", at(0))
        .unwrap()
        .id
}

/// **A second workspace holding the same bytes is not read again** — whether
/// both are queued together or the second arrives after the first was read
/// and mirrored — and each workspace finds the file by its words.
#[test]
fn a_second_workspace_holding_the_same_bytes_is_not_read_again() {
    let pdf = hash(40);
    let library = library(&[(
        &pdf,
        Script::Pages(vec![(1, "shared okapiword")], ReadingStatus::Read, None),
    )]);
    let mut store = store();
    let (one, two, three) = (
        workspace(&mut store),
        second_workspace(&mut store),
        second_workspace(&mut store),
    );
    let first = a_file(&mut store, one, "Shared.pdf", &pdf, "application/pdf");
    let second = a_file(&mut store, two, "Shared.pdf", &pdf, "application/pdf");
    land(&mut store, one, &pdf, 10, "application/pdf");
    land(&mut store, two, &pdf, 10, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    device.settle();

    assert_eq!(device.reads(), 1, "read once for two workspaces");
    assert_eq!(device.coordinator.stats.copied, 1);
    assert_eq!(
        device.reading(two, second, &pdf),
        device.reading(one, first, &pdf),
        "the same reading in both"
    );
    assert!(device.found("okapiword", first));
    assert!(device.found("okapiword", second));

    // A third, later: copied from what the mirror holds.
    let third = {
        let mut store = device.store();
        let third = a_file(&mut store, three, "Shared.pdf", &pdf, "application/pdf");
        land(&mut store, three, &pdf, 10, "application/pdf");
        third
    };
    device.settle();
    assert_eq!(device.reads(), 1, "still read once");
    assert_eq!(device.coordinator.stats.copied, 2);
    assert!(device.found("okapiword", third));
}

/// **A recompute between two entries of the same bytes does not make the
/// second a read** — the case the real run found (2 of 46 shared files read
/// twice): the mirror applies on its own thread, so when the queue is
/// recomputed after the first entry is read and before the mirror holds it,
/// only what the coordinator itself wrote can say the words exist.
#[test]
fn a_recompute_before_the_mirror_catches_up_does_not_read_the_same_bytes_twice() {
    let pdf = hash(42);
    let library = library(&[(
        &pdf,
        Script::Pages(vec![(1, "shared tapirword")], ReadingStatus::Read, None),
    )]);
    let mut store = store();
    let (one, two) = (workspace(&mut store), second_workspace(&mut store));
    a_file(&mut store, one, "Shared.pdf", &pdf, "application/pdf");
    a_file(&mut store, two, "Shared.pdf", &pdf, "application/pdf");
    land(&mut store, one, &pdf, 10, "application/pdf");
    land(&mut store, two, &pdf, 10, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    project(&mut device);
    // No applier step anywhere below: the mirror stays behind.
    assert!(device.coordinator.step(device.now), "the first entry");
    assert_eq!(device.reads(), 1);
    device.coordinator.recompute(device.now);
    while device.coordinator.step(device.now) {}
    assert_eq!(device.reads(), 1, "the second entry was copied, not read");
    assert_eq!(device.coordinator.stats.copied, 1);
}

/// **A fuller reading reaches its siblings** (TECHNICAL §18, the review's
/// note): when one workspace's capped `partial` becomes `read` — what *Read
/// all pages* will do — the other workspace holding the same bytes is given
/// the `read` entry, copied and not read; and when the reading's generation
/// rises, the sibling is given that too. Once level, nothing moves again.
#[test]
fn a_fuller_reading_reaches_the_sibling_holding_the_same_bytes() {
    let pdf = hash(41);
    let library = library(&[(
        &pdf,
        Script::Pages(
            vec![(1, "first page alphaword")],
            ReadingStatus::Partial,
            Some(Reason::PageCap),
        ),
    )]);
    let mut store = store();
    let (one, two) = (workspace(&mut store), second_workspace(&mut store));
    let first = a_file(&mut store, one, "Long.pdf", &pdf, "application/pdf");
    let second = a_file(&mut store, two, "Long.pdf", &pdf, "application/pdf");
    land(&mut store, one, &pdf, 10, "application/pdf");
    land(&mut store, two, &pdf, 10, "application/pdf");
    let mut device = Device::new(store, stub(library));
    device.here(&pdf);
    device.settle();
    assert_eq!(device.reads(), 1);
    let capped = device.reading(two, second, &pdf).expect("copied");
    assert_eq!(capped.reason, Some(Reason::PageCap));
    assert!(!device.found("betaword", second));

    // The fuller reading arrives in the first workspace, as *Read all pages*
    // there would write it; the second is the sibling that should receive it.
    let (read_ws, read_parent, sibling_ws, sibling_parent) = (one, first, two, second);
    let write_into = |device: &Device, reading: &Reading| {
        let document = reading_document_id(read_parent);
        let mut store = device.store();
        let fork = store.fork_document(read_ws, document).unwrap();
        let before = fork.oplog_vv();
        let doc = ReadingDoc::from_doc(fork);
        doc.write(&pdf, reading).unwrap();
        let delta = doc.export_updates_since(&before).unwrap();
        store
            .apply_update(read_ws, document, delta, at(20), UpdateOrigin::Store)
            .unwrap();
        store.flush(read_ws, document, at(20)).unwrap();
    };

    let mut full = capped.clone();
    full.status = Some(ReadingStatus::Read);
    full.reason = None;
    full.pages_done = None;
    full.pages
        .insert(2, PageText::new("second page betaword, past the cap"));
    write_into(&device, &full);
    device.settle();
    assert_eq!(device.reads(), 1, "the sibling is copied, never read");
    assert_eq!(device.coordinator.stats.refreshed, 1);
    let given = device
        .reading(sibling_ws, sibling_parent, &pdf)
        .expect("held");
    assert_eq!(given.status, Some(ReadingStatus::Read));
    assert_eq!(given.pages.len(), 2);
    assert!(device.found("betaword", sibling_parent));

    let mut newer = full.clone();
    newer.generation = GENERATION + 1;
    newer
        .pages
        .insert(3, PageText::new("a better reader found gammaword"));
    write_into(&device, &newer);
    device.settle();
    assert_eq!(device.reads(), 1);
    assert_eq!(device.coordinator.stats.refreshed, 2);
    assert_eq!(
        device
            .reading(sibling_ws, sibling_parent, &pdf)
            .map(|r| r.generation),
        Some(GENERATION + 1)
    );

    // Level now: a recompute queues nothing for either.
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(device.coordinator.stats.refreshed, 2, "nothing moves again");
}

// ------------------------------------------------------------- battery saver

/// A power state the test sets.
struct Saver(Arc<AtomicBool>);

impl power::Power for Saver {
    fn saving(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// **The backlog waits out battery saver; a fresh file does not**: nothing of
/// the backlog is read while the system saves energy, a file added meanwhile
/// is read at once, the debug board says why the rest waits, and when saving
/// ends the backlog goes on.
#[test]
fn the_backlog_pauses_under_battery_saver_and_a_fresh_file_does_not() {
    let mut store = store();
    let ws = workspace(&mut store);
    let (backlog, by_path) = import(&mut store, ws, 3, 0);
    let saving = Arc::new(AtomicBool::new(true));
    let mut device = Device::built(
        store,
        any_file(),
        None,
        Some(Box::new(Saver(Arc::clone(&saving)))),
    );
    for h in &backlog {
        device.here(h);
    }
    device.settle();
    assert_eq!(device.reads(), 0, "the backlog waits");
    assert!(device.coordinator.paused);
    assert_eq!(device.coordinator.counts.backlog, 3);
    let report = coordinator_report(&device.coordinator).to_string();
    assert!(report.contains("battery-saver"), "{report}");

    let fresh = nth(500);
    device.here(&fresh);
    {
        let mut store = device.store();
        a_file(&mut store, ws, "New.pdf", &fresh, "application/pdf");
        land(&mut store, ws, &fresh, 99_000_000, "application/pdf");
    }
    device.settle();
    assert_eq!(device.reads(), 1, "the fresh file is read");
    let asked = device.requests.lock().unwrap().clone();
    assert!(
        matches!(&asked[0], Request::Read { path, .. } if *path == object_path(&fresh)),
        "the one read was the fresh file"
    );
    assert!(device.coordinator.paused, "and the backlog still waits");

    saving.store(false, Ordering::SeqCst);
    device.settle();
    assert_eq!(device.reads(), 4, "saving ended: the backlog goes on");
    assert!(!device.coordinator.paused);
    let order = read_order(&device, &by_path);
    assert!(order.windows(2).all(|w| w[0] <= w[1]));
    let report = coordinator_report(&device.coordinator).to_string();
    assert!(!report.contains("battery-saver"), "{report}");
}
