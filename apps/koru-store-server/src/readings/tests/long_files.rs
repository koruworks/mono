// SPDX-License-Identifier: MPL-2.0

//! **Long files** (OCR ROADMAP Plan 3.2; TECHNICAL D12, §9 "Read all pages",
//! "Read again", §18): a long scan stops at the reading cap as partly read and
//! is not read again; *Read all pages* resumes it with no cap, a window at a
//! time, behind anything freshly added and ahead of the backlog, and yields to
//! a file added meanwhile between two of its pages; a PDF stopped at the
//! text-layer cap is finished with no reader installed; *Read again* writes an
//! entry again at the same generation; a request is not met where the bytes
//! are absent, and its row goes when it has been met or its item has gone.
//!
//! Through the same in-memory device as the rest of the plan's verification,
//! with a stub worker that keeps the real worker's caps: no disk, no process,
//! no model.

use koru_read::Want;

use super::coordinator::{REQUEST_READ_WINDOW, REQUEST_TEXT_WINDOW};
use super::*;

/// A PDF the stub worker reads: how many pages, whether they are scans (each
/// needs the reader), and the word every page carries with its number.
#[derive(Clone)]
struct Doc {
    pages: u32,
    scanned: bool,
    word: &'static str,
}

/// The word on page `n` of a document whose word is `word`: letters only, so
/// the index takes it as one token (`lynx` page 280 is `lynxcib`).
fn word_on(word: &str, n: u32) -> String {
    let mut tag = String::new();
    let mut n = n;
    loop {
        tag.insert(0, char::from(b'a' + (n % 26) as u8));
        n /= 26;
        if n == 0 {
            break;
        }
    }
    format!("{word}{tag}")
}

type Docs = Arc<Mutex<HashMap<String, Doc>>>;

/// **A worker that keeps the real one's caps** (`apps/koru-read/src/job.rs`):
/// the text cap counts pages from `from_page`; the reading cap counts pages
/// sent to the reader and stops *before* the one past it, so a resume at
/// `pages_done + 1` starts there; a scan asked for text only leaves its pages
/// for the reader and ends `partial` with no reason.
fn paged(docs: Docs, built: bool) -> StubLauncher {
    let mut stub = StubLauncher::new(move |request: &Request| {
        let Request::Read {
            job,
            path,
            from_page,
            max_text_pages,
            max_read_pages,
            want,
            ..
        } = request
        else {
            return Ok(Vec::new());
        };
        let (doc, _) = docs
            .lock()
            .unwrap()
            .iter()
            .find(|(hash, _)| object_path(hash) == *path)
            .map(|(hash, doc)| (doc.clone(), hash.clone()))
            .expect("a scripted file");
        let recognize = *want == Want::Recognize;
        let first = (*from_page).max(1);
        let last = match max_text_pages {
            Some(cap) => doc.pages.min((first - 1).saturating_add(*cap)),
            None => doc.pages,
        };
        let page = |n: u32, text: String, source: Source, confidence: Option<u8>| Event::Page {
            job: *job,
            page: n,
            text: PageText::new(text),
            source,
            confidence,
        };
        let mut events = Vec::new();
        let (mut recognized, mut left, mut stopped) = (0_u32, 0_u32, None);
        for n in first..=last {
            if !doc.scanned {
                events.push(page(n, word_on(doc.word, n), Source::TextLayer, None));
                continue;
            }
            if !recognize {
                left += 1;
                events.push(page(n, String::new(), Source::TextLayer, None));
                continue;
            }
            if max_read_pages.is_some_and(|cap| recognized >= cap) {
                stopped = Some(n - 1);
                break;
            }
            recognized += 1;
            events.push(page(n, word_on(doc.word, n), Source::Recognized, Some(90)));
        }
        let (status, reason) = if stopped.is_some() {
            (ReadingStatus::Partial, Some(Reason::PageCap))
        } else if left > 0 {
            (ReadingStatus::Partial, None)
        } else if last < doc.pages {
            (ReadingStatus::Partial, Some(Reason::PageCap))
        } else {
            (ReadingStatus::Read, None)
        };
        events.push(Event::Done {
            job: *job,
            status,
            reason,
            pages_done: stopped.unwrap_or(last),
        });
        Ok(events)
    });
    stub.recognition_built = built;
    stub
}

fn docs(entries: &[(&str, Doc)]) -> Docs {
    Arc::new(Mutex::new(
        entries
            .iter()
            .map(|(hash, doc)| ((*hash).to_owned(), doc.clone()))
            .collect(),
    ))
}

/// **A person chooses *Read all pages* or *Read again***, as the route records
/// it: the row, then the wake.
fn ask(device: &Device, ws: WorkspaceId, hash: &str) {
    let mut mirror = device.derived.mirror().unwrap();
    assert!(mirror.request(ws, hash, device.now).unwrap(), "recorded");
    device.inbox.read_requested(ws, hash);
}

fn requested(device: &Device, ws: WorkspaceId, hash: &str) -> bool {
    device
        .derived
        .mirror()
        .unwrap()
        .requested(ws, hash)
        .unwrap()
}

/// `(from_page, max_text_pages, max_read_pages, want)` of every job for `hash`.
fn jobs(device: &Device, hash: &str) -> Vec<(u32, Option<u32>, Option<u32>, Want)> {
    requests_for(device, hash)
        .into_iter()
        .map(|request| match request {
            Request::Read {
                from_page,
                max_text_pages,
                max_read_pages,
                want,
                ..
            } => (from_page, max_text_pages, max_read_pages, want),
            _ => unreachable!(),
        })
        .collect()
}

/// A device with the reader installed and a worker that recognizes.
fn recognizing(store: KoruStore, docs: Docs) -> (Device, Arc<ReaderHub>) {
    let hub = desktop_hub();
    hub.set_installed(Some(installed_model()));
    let device = Device::with_hub(store, paged(docs, true), Some(Arc::clone(&hub)));
    (device, hub)
}

/// The projector and the mirror catch up, with no coordinator step: a file
/// added now reaches the coordinator's inbox as it would in the daemon.
fn project(device: &mut Device) {
    for _ in 0..5 {
        device.projector.tick(device.now);
        device.applier.step();
        device.now += QUIET_MILLIS + 1;
    }
}

// --------------------------------------------------------------- the cap

/// **A 300-page scan stops at the reading cap, partly read** (D12): written
/// `partial`, `page-cap`, with how far it got; found by its first pages and
/// not by page 280; counted as partly read; settled, so it is not read again.
#[test]
fn a_long_scan_stops_at_the_reading_cap_and_is_partly_read() {
    let scan = hash(60);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    land(&mut store, ws, &scan, 30_000_000, "application/pdf");
    let library = docs(&[(
        &scan,
        Doc {
            pages: 300,
            scanned: true,
            word: "lynx",
        },
    )]);
    let (mut device, hub) = recognizing(store, library);
    device.here(&scan);
    device.settle();

    assert_eq!(
        jobs(&device, &scan),
        vec![(1, Some(2_000), Some(200), Want::Recognize)],
        "one job, with the product caps"
    );
    let reading = device.reading(ws, file, &scan).expect("written");
    assert_eq!(reading.status, Some(ReadingStatus::Partial));
    assert_eq!(reading.reason, Some(Reason::PageCap));
    assert_eq!(reading.pages_done, Some(200));
    assert_eq!(reading.pages.len(), 200);
    assert!(device.found(&word_on("lynx", 5), file), "an early page");
    assert!(!device.found(&word_on("lynx", 280), file), "past the cap");
    let counts = hub.state().counts;
    assert_eq!((counts.partly_read, counts.unread), (1, 0));
    assert_eq!(
        device
            .derived
            .mirror()
            .unwrap()
            .partly_read()
            .unwrap()
            .len(),
        1
    );

    // Settled: told again, recomputed, nothing is read.
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(jobs(&device, &scan).len(), 1, "not retried");
}

// ------------------------------------------------------- Read all pages

/// **Read all pages resumes at `pages_done` with no cap, a page at a time,
/// and ends `read`** (D12, §9): each window is written as `partial` with no
/// reason; the last writes `read`; page 280 is found; the item is no longer
/// partly read and the request row is gone.
#[test]
fn read_all_pages_resumes_a_window_at_a_time_and_ends_read() {
    let scan = hash(61);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    land(&mut store, ws, &scan, 30_000_000, "application/pdf");
    let library = docs(&[(
        &scan,
        Doc {
            pages: 300,
            scanned: true,
            word: "lynx",
        },
    )]);
    let (mut device, hub) = recognizing(store, library);
    device.here(&scan);
    device.settle();
    assert_eq!(jobs(&device, &scan).len(), 1);

    ask(&device, ws, &scan);
    // One window, then look: progress is `partial` with no reason.
    project(&mut device);
    assert!(device.coordinator.step(device.now));
    let going = device.reading(ws, file, &scan).unwrap();
    assert_eq!(going.status, Some(ReadingStatus::Partial));
    assert_eq!(going.reason, None, "progress, not a cap");
    assert_eq!(going.pages_done, Some(201));
    assert!(requested(&device, ws, &scan), "not met yet");

    device.settle();
    let all = jobs(&device, &scan);
    assert_eq!(all.len(), 1 + 100, "the first 200, then one page a window");
    assert_eq!(
        all[1],
        (
            201,
            Some(REQUEST_TEXT_WINDOW),
            Some(REQUEST_READ_WINDOW),
            Want::Recognize
        ),
        "resumed at pages_done + 1, a window wide"
    );
    assert_eq!(all[100].0, 300, "the last window is the last page");
    assert!(
        all.iter()
            .skip(1)
            .zip(201..)
            .all(|(job, page)| job.0 == page),
        "every window starts where the last stopped"
    );
    let reading = device.reading(ws, file, &scan).unwrap();
    assert_eq!(reading.status, Some(ReadingStatus::Read));
    assert_eq!(reading.reason, None);
    assert_eq!(reading.pages.len(), 300);
    assert_eq!(reading.generation, GENERATION);
    assert!(device.found(&word_on("lynx", 280), file), "page 280");
    assert!(device.found(&word_on("lynx", 5), file), "and still page 5");
    assert!(
        !requested(&device, ws, &scan),
        "the request row goes when met"
    );
    assert_eq!(device.coordinator.stats.windows, 99);
    assert_eq!(device.coordinator.stats.requests_met, 1);
    let counts = hub.state().counts;
    assert_eq!(
        (counts.partly_read, counts.read, counts.unread),
        (0, 1, 0),
        "no longer partly read"
    );
    assert!(
        device
            .derived
            .mirror()
            .unwrap()
            .partly_read()
            .unwrap()
            .is_empty()
    );

    // Met: nothing is read again.
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(jobs(&device, &scan).len(), 101);
}

/// **A request is queued behind anything freshly added and ahead of the
/// backlog** (§9), whatever the sizes say: the order the worker is asked in
/// is the new file, then every window of the requested scan, then the
/// backlog's file — the smallest of the three.
#[test]
fn a_request_is_queued_behind_fresh_files_and_ahead_of_the_backlog() {
    let (scan, old, new) = (hash(62), hash(63), hash(64));
    let mut store = store();
    let ws = workspace(&mut store);
    a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    a_file(&mut store, ws, "Old.pdf", &old, "application/pdf");
    // The backlog's file is the smallest: smallest first would put it first.
    land(&mut store, ws, &scan, 30_000_000, "application/pdf");
    land(&mut store, ws, &old, 10, "application/pdf");
    let library = docs(&[
        (
            &scan,
            Doc {
                pages: 300,
                scanned: true,
                word: "lynx",
            },
        ),
        (
            &old,
            Doc {
                pages: 1,
                scanned: false,
                word: "old",
            },
        ),
        (
            &new,
            Doc {
                pages: 1,
                scanned: false,
                word: "new",
            },
        ),
    ]);
    let (mut device, _hub) = recognizing(store, library);
    device.here(&scan);
    device.settle();
    assert_eq!(device.reads(), 1, "the scan, capped; `old` is not here yet");

    // `old`'s bytes are here now (the backlog will find it); the scan is
    // asked for; a new file is added — all before the coordinator looks.
    device.here(&old);
    ask(&device, ws, &scan);
    device.here(&new);
    {
        let mut store = device.store();
        a_file(&mut store, ws, "New.pdf", &new, "application/pdf");
        land(&mut store, ws, &new, 20_000_000, "application/pdf");
    }
    project(&mut device);
    assert!(device.coordinator.step(device.now));
    let order: Vec<(String, bool, bool)> = device
        .coordinator
        .queued()
        .into_iter()
        .map(|p| (p.hash, p.fresh, p.requested))
        .collect();
    assert!(
        order
            .first()
            .is_some_and(|(h, _, requested)| *h == scan && *requested),
        "after the new file, the request heads the queue: {order:?}"
    );
    device.settle();
    let paths: Vec<PathBuf> = device
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            Request::Read { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let first = |h: &str| paths.iter().position(|p| *p == object_path(h)).unwrap();
    let last = |h: &str| paths.iter().rposition(|p| *p == object_path(h)).unwrap();
    assert_eq!(first(&new), 1, "the new file first: {paths:?}");
    assert!(
        last(&scan) < first(&old),
        "every window of the request before the backlog"
    );
    assert_eq!(paths.len(), 1 + 1 + 100 + 1);
}

/// **It yields between pages** (§9): a file added while *Read all pages*
/// runs is read before the long file's next page, and the long file then
/// carries on where it stopped.
#[test]
fn a_file_added_while_read_all_runs_is_read_before_the_next_page() {
    let (scan, shot) = (hash(65), hash(66));
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    land(&mut store, ws, &scan, 30_000_000, "application/pdf");
    let library = docs(&[
        (
            &scan,
            Doc {
                pages: 300,
                scanned: true,
                word: "lynx",
            },
        ),
        (
            &shot,
            Doc {
                pages: 1,
                scanned: false,
                word: "shot",
            },
        ),
    ]);
    let (mut device, _hub) = recognizing(store, library);
    device.here(&scan);
    device.settle();

    ask(&device, ws, &scan);
    project(&mut device);
    assert!(device.coordinator.step(device.now), "window 201");
    assert!(device.coordinator.step(device.now), "window 202");

    // A screenshot is added while the scan is being read in full.
    device.here(&shot);
    let page = {
        let mut store = device.store();
        let page = a_file(&mut store, ws, "Shot.pdf", &shot, "application/pdf");
        land(&mut store, ws, &shot, 5_000, "application/pdf");
        page
    };
    project(&mut device);
    assert!(device.coordinator.step(device.now), "the next job");
    let paths: Vec<PathBuf> = device
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            Request::Read { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        paths,
        vec![
            object_path(&scan),
            object_path(&scan),
            object_path(&scan),
            object_path(&shot)
        ],
        "the capped read, two windows, then the new file before page 203"
    );
    device.settle();
    let scan_jobs = jobs(&device, &scan);
    assert_eq!(scan_jobs[3].0, 203, "and the scan carries on at page 203");
    assert!(device.found(&word_on("shot", 1), page));
    let reading = device.reading(ws, file, &scan).unwrap();
    assert_eq!(reading.status, Some(ReadingStatus::Read));
}

/// **Read all pages on a PDF stopped at the text-layer cap needs no reader**
/// (§18): with no model installed the request goes on with text-only jobs,
/// [`REQUEST_TEXT_WINDOW`] pages at a time, and ends `read`.
#[test]
fn read_all_on_a_text_capped_pdf_is_finished_with_no_reader() {
    let long = hash(67);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Manual.pdf", &long, "application/pdf");
    land(&mut store, ws, &long, 9_000_000, "application/pdf");
    let library = docs(&[(
        &long,
        Doc {
            pages: 2_500,
            scanned: false,
            word: "otter",
        },
    )]);
    // A desktop with no reader installed: the worker reads text only.
    let hub = desktop_hub();
    let mut device = Device::with_hub(store, paged(library, false), Some(Arc::clone(&hub)));
    device.here(&long);
    device.settle();
    let capped = device.reading(ws, file, &long).unwrap();
    assert_eq!(capped.reason, Some(Reason::PageCap));
    assert_eq!(capped.pages_done, Some(2_000));
    assert!(!device.found(&word_on("otter", 2_400), file));

    ask(&device, ws, &long);
    device.settle();
    assert_eq!(
        jobs(&device, &long),
        vec![
            (1, Some(2_000), Some(200), Want::Text),
            (2_001, Some(200), Some(1), Want::Text),
            (2_201, Some(200), Some(1), Want::Text),
            (2_401, Some(200), Some(1), Want::Text),
        ],
        "text only, a window at a time, never the reader"
    );
    let reading = device.reading(ws, file, &long).unwrap();
    assert_eq!(reading.status, Some(ReadingStatus::Read));
    assert_eq!(reading.reader.as_ref().map(ReaderId::as_str), Some("text"));
    assert!(device.found(&word_on("otter", 2_400), file));
    assert!(!requested(&device, ws, &long));
    assert!(hub.state().installed.is_none(), "no model was involved");
}

/// **A request is not met where the bytes are absent** (§9, §11): the item
/// is counted as it was, nothing is read, and the request waits for the bytes.
#[test]
fn a_request_whose_bytes_are_absent_reads_nothing() {
    let scan = hash(68);
    let mut store = store();
    let ws = workspace(&mut store);
    a_file(&mut store, ws, "Scan.pdf", &scan, "application/pdf");
    land(&mut store, ws, &scan, 30_000_000, "application/pdf");
    let library = docs(&[(
        &scan,
        Doc {
            pages: 300,
            scanned: true,
            word: "lynx",
        },
    )]);
    let (mut device, hub) = recognizing(store, library);
    device.here(&scan);
    device.settle();
    // The bytes leave this device (space reclaimed); the request is made
    // anyway, as a stale surface might.
    device.present.lock().unwrap().remove(&scan);
    ask(&device, ws, &scan);
    device.coordinator.recompute(device.now);
    device.settle();
    assert_eq!(jobs(&device, &scan).len(), 1, "nothing read");
    assert!(requested(&device, ws, &scan), "it waits for the bytes");
    assert_eq!(hub.state().counts.partly_read, 1, "still partly read");
}

// ------------------------------------------------------------ Read again

/// **Read again writes the entry again at the same generation** (§7, §9):
/// the words the worker gives now replace the ones it gave before, under the
/// same keys; the generation does not move; the request goes when met.
#[test]
fn read_again_writes_the_entry_again_at_the_same_generation() {
    let pdf = hash(69);
    let mut store = store();
    let ws = workspace(&mut store);
    let file = a_file(&mut store, ws, "Short.pdf", &pdf, "application/pdf");
    land(&mut store, ws, &pdf, 4_000, "application/pdf");
    let library = docs(&[(
        &pdf,
        Doc {
            pages: 3,
            scanned: false,
            word: "heron",
        },
    )]);
    let mut device = Device::new(store, paged(Arc::clone(&library), false));
    device.here(&pdf);
    device.settle();
    assert!(device.found(&word_on("heron", 2), file));
    let before = device.reading(ws, file, &pdf).unwrap();

    // The worker would read these bytes differently now (a better reader, a
    // fixed bug): what *Read again* is for.
    library.lock().unwrap().get_mut(&pdf).unwrap().word = "egret";
    ask(&device, ws, &pdf);
    device.settle();
    assert_eq!(
        jobs(&device, &pdf).last().map(|j| j.0),
        Some(1),
        "read again from the first page"
    );
    let after = device.reading(ws, file, &pdf).unwrap();
    assert_eq!(after.generation, before.generation, "the same generation");
    assert_eq!(after.status, Some(ReadingStatus::Read));
    assert_eq!(after.pages.len(), 3, "the same keys, written again");
    assert!(device.found(&word_on("egret", 2), file));
    assert!(
        !device.found(&word_on("heron", 2), file),
        "the old words go"
    );
    assert!(!requested(&device, ws, &pdf));
}

/// **Read again on a picture inside a page** reads that picture again with
/// the reader, at the same generation.
#[test]
fn read_again_on_a_picture_reads_it_again() {
    let picture = hash(70);
    let mut store = store();
    let ws = workspace(&mut store);
    let page = a_page_with_picture(&mut store, ws, &picture);
    land(&mut store, ws, &picture, 900, "image/png");
    let library = docs(&[(
        &picture,
        Doc {
            pages: 1,
            scanned: true,
            word: "kestrel",
        },
    )]);
    let (mut device, _hub) = recognizing(store, Arc::clone(&library));
    device.here(&picture);
    device.settle();
    assert!(device.found(&word_on("kestrel", 1), page));
    library.lock().unwrap().get_mut(&picture).unwrap().word = "merlin";
    ask(&device, ws, &picture);
    device.settle();
    let all = jobs(&device, &picture);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].3, Want::Recognize);
    assert_eq!(
        (all[1].1, all[1].2),
        (Some(2_000), Some(200)),
        "a picture is one ordinary job"
    );
    assert!(device.found(&word_on("merlin", 1), page));
    assert!(!requested(&device, ws, &picture));
}

/// **A request whose item has gone is dropped** at the next recompute: no
/// row is left standing for something nobody can read.
#[test]
fn a_request_for_an_item_that_is_gone_is_dropped() {
    let mut store = store();
    let ws = workspace(&mut store);
    let device_store = store;
    let mut device = Device::new(device_store, paged(docs(&[]), false));
    device.settle();
    let gone = hash(71);
    let mut mirror = device.derived.mirror().unwrap();
    mirror.request(ws, &gone, 0).unwrap();
    assert!(requested(&device, ws, &gone));
    device.coordinator.recompute(device.now);
    assert!(!requested(&device, ws, &gone), "no item holds these bytes");
}

// ------------------------------------------------- offers and refusals

fn facts(status: &str, reason: Option<&str>, source: &str, reader: &str, mime: &str) -> Facts {
    Facts {
        status: Some(status.into()),
        reason: reason.map(str::to_owned),
        source: Some(source.into()),
        reader: Some(reader.into()),
        mime: Some(mime.into()),
        here: true,
        reader_ready: true,
        pending: false,
    }
}

/// **What the route offers is what it accepts, and each refusal has its
/// code** (§10, §18): the bytes absent; the reader absent where finishing
/// needs it — but not for a PDF stopped at the text-layer cap; nothing to
/// ask for; a request already waiting.
#[test]
fn the_offers_and_the_refusals_are_one_decision() {
    use koru_store_core::errors::Code;
    use koru_store_wire::readings::{ReadRequestKind, ReadingOffers};
    let code = |facts: &Facts, kind| facts.refusal(kind).map(|e| e.code);

    let scan_cap = facts(
        "partial",
        Some("page-cap"),
        "recognized",
        "pp-ocr",
        "application/pdf",
    );
    assert_eq!(code(&scan_cap, ReadRequestKind::ReadAll), None);
    assert_eq!(
        scan_cap.offers(),
        ReadingOffers {
            read_all: true,
            read_again: false
        },
        "partly read is not read: only Read all pages"
    );
    let not_here = Facts {
        here: false,
        ..scan_cap.clone()
    };
    assert_eq!(
        code(&not_here, ReadRequestKind::ReadAll),
        Some(Code::BlobNotHere)
    );
    assert!(!not_here.offers().read_all);
    let no_reader = Facts {
        reader_ready: false,
        ..scan_cap.clone()
    };
    assert_eq!(
        code(&no_reader, ReadRequestKind::ReadAll),
        Some(Code::ReaderUnavailable)
    );

    // Stopped at the text-layer cap, every word from text layers: no reader.
    let text_cap = Facts {
        reader_ready: false,
        ..facts(
            "partial",
            Some("page-cap"),
            "text-layer",
            "text",
            "application/pdf",
        )
    };
    assert_eq!(code(&text_cap, ReadRequestKind::ReadAll), None);
    assert!(text_cap.offers().read_all);

    let waiting = Facts {
        pending: true,
        ..scan_cap.clone()
    };
    assert_eq!(
        code(&waiting, ReadRequestKind::ReadAll),
        Some(Code::InvalidRequest)
    );
    assert!(!waiting.offers().read_all, "not offered twice");

    // A plain-text file cut at its size limit has no pages to go on with.
    let plain_cap = facts("partial", Some("page-cap"), "plain", "text", "text/plain");
    assert_eq!(
        code(&plain_cap, ReadRequestKind::ReadAll),
        Some(Code::InvalidRequest)
    );
    let still_going = facts("partial", None, "recognized", "pp-ocr", "application/pdf");
    assert_eq!(still_going.offers(), ReadingOffers::default());

    // Read again: read, empty, failed — a picture needs the reader.
    for status in ["read", "empty", "failed"] {
        let text = facts(status, None, "text-layer", "text", "application/pdf");
        assert_eq!(code(&text, ReadRequestKind::ReadAgain), None, "{status}");
        assert!(!text.offers().read_all, "{status}");
    }
    let picture = facts("read", None, "recognized", "pp-ocr", "image/png");
    assert_eq!(code(&picture, ReadRequestKind::ReadAgain), None);
    let picture_no_reader = Facts {
        reader_ready: false,
        ..picture.clone()
    };
    assert_eq!(
        code(&picture_no_reader, ReadRequestKind::ReadAgain),
        Some(Code::ReaderUnavailable)
    );
    let picture_not_here = Facts {
        here: false,
        ..picture
    };
    assert_eq!(
        code(&picture_not_here, ReadRequestKind::ReadAgain),
        Some(Code::BlobNotHere)
    );
    // Nothing read yet: nothing to ask for.
    assert_eq!(
        code(&Facts::default(), ReadRequestKind::ReadAgain),
        Some(Code::InvalidRequest)
    );
}
