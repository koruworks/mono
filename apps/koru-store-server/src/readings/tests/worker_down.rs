// SPDX-License-Identifier: MPL-2.0

//! **A worker that cannot be asked right now is not a reader that is gone**
//! (OCR Plan 3.6).
//!
//! The big run's first pass on the generated archive (MEASUREMENTS §7,
//! QUESTIONS Q30): mid-backlog, after a restart, the worker went silent on a
//! page, the Store's patience ended it, and the coordinator then called the
//! backlog finished with 76 scans counted unread, `needs_reader` at 0 and
//! nothing queued. Through the same in-memory device as the rest: no disk,
//! no process.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

/// Made-up words, one per file, found only by recognition.
const WORDS: [&str; 6] = [
    "quillfrond",
    "marrowgast",
    "teaselnook",
    "brindlecap",
    "fennowisp",
    "galdermoth",
];

/// What the backlog holds.
#[derive(Copy, Clone)]
enum Kind {
    /// Two-page scans: a typed cover page in a text layer, and a page of
    /// pixels carrying the scan's word.
    Scans,
    /// Pictures in pages: all pixels.
    Pictures,
}

type Files = Vec<(String, DocumentId, &'static str)>;

/// Six files, smallest first in the order of `WORDS`.
fn files(store: &mut KoruStore, ws: WorkspaceId, kind: Kind) -> Files {
    WORDS
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let h = hash(60 + i as u8);
            let size = 100 + i as u64;
            let document = match kind {
                Kind::Scans => {
                    let file = a_file(store, ws, "Scan.pdf", &h, "application/pdf");
                    land(store, ws, &h, size, "application/pdf");
                    file
                }
                Kind::Pictures => {
                    let page = a_page_with_picture(store, ws, &h);
                    land(store, ws, &h, size, "image/png");
                    page
                }
            };
            (h, document, *word)
        })
        .collect()
}

/// How the stub worker misbehaves, set by the test as it goes.
#[derive(Clone, Default)]
struct Trouble {
    /// The next recognition job goes silent: the real launcher ends a worker
    /// past its patience and reports the job `crashed`, and the next workers
    /// do not answer their status for a while — `outage` probes.
    hang: Arc<AtomicBool>,
    /// Every recognition job answers `needs-reader`, as a worker whose model
    /// would not load does; the stub then says `recognition: false`.
    will_not_load: Arc<AtomicBool>,
}

/// **A stub worker that reads scans and pictures like the real one**: a text
/// job finds the cover page and leaves the pixels for the reader (a picture
/// answers `needs-reader`); a recognition job finds the word.
fn reader(files: &Files, trouble: &Trouble, outage: usize) -> StubLauncher {
    let words: HashMap<PathBuf, &'static str> = files
        .iter()
        .map(|(h, _, word)| (object_path(h), *word))
        .collect();
    let failing: Arc<AtomicUsize> = Arc::default();
    let after_hang = Arc::clone(&failing);
    let trouble = trouble.clone();
    let mut stub = StubLauncher::new(move |request: &Request| {
        let Request::Read {
            job,
            path,
            mime,
            from_page,
            want,
            ..
        } = request
        else {
            return Ok(Vec::new());
        };
        let word = *words.get(path).expect("a scripted file");
        let recognize = *want == koru_read::Want::Recognize;
        if recognize && trouble.hang.swap(false, Ordering::Relaxed) {
            after_hang.store(outage, Ordering::Relaxed);
            return Err(Interrupted::Crashed {
                memory_limit: false,
            });
        }
        let picture = mime.starts_with("image/");
        if (recognize && trouble.will_not_load.load(Ordering::Relaxed)) || (picture && !recognize) {
            return Ok(vec![Event::Failed {
                job: *job,
                code: FailureCode::NeedsReader,
            }]);
        }
        let pixels = |page: u32| {
            if recognize {
                Event::Page {
                    job: *job,
                    page,
                    text: PageText::new(word),
                    source: Source::Recognized,
                    confidence: Some(90),
                }
            } else {
                Event::Page {
                    job: *job,
                    page,
                    text: PageText::new(""),
                    source: Source::TextLayer,
                    confidence: None,
                }
            }
        };
        let mut events = Vec::new();
        if picture {
            events.push(pixels(1));
        } else {
            if *from_page <= 1 {
                events.push(Event::Page {
                    job: *job,
                    page: 1,
                    text: PageText::new("typed cover page"),
                    source: Source::TextLayer,
                    confidence: None,
                });
            }
            events.push(pixels(2));
        }
        events.push(Event::Done {
            job: *job,
            status: if recognize {
                ReadingStatus::Read
            } else {
                ReadingStatus::Partial
            },
            reason: None,
            pages_done: if picture { 1 } else { 2 },
        });
        Ok(events)
    });
    stub.recognition_built = true;
    stub.failing_probes = failing;
    stub
}

/// A device holding six files of `kind`, read once with no reader installed:
/// each waits for the reader.
struct Waiting {
    device: Device,
    hub: Arc<ReaderHub>,
    ws: WorkspaceId,
    files: Files,
    trouble: Trouble,
    /// Probes left to fail, shared with the stub.
    failing: Arc<AtomicUsize>,
}

impl Waiting {
    fn new(kind: Kind, outage: usize) -> Waiting {
        let mut store = store();
        let ws = workspace(&mut store);
        let files = files(&mut store, ws, kind);
        let hub = desktop_hub();
        let trouble = Trouble::default();
        let launcher = reader(&files, &trouble, outage);
        let failing = Arc::clone(&launcher.failing_probes);
        let mut device = Device::with_hub(store, launcher, Some(Arc::clone(&hub)));
        for (h, ..) in &files {
            device.here(h);
        }
        device.settle();
        let waiting = Waiting {
            device,
            hub,
            ws,
            files,
            trouble,
            failing,
        };
        assert_eq!(
            waiting.counts(),
            (6, 6),
            "the text pass left every file for the reader"
        );
        waiting
    }

    /// `(unread, needs_reader)` as a search in the workspace sees them.
    fn counts(&self) -> (u64, u64) {
        let counts = self.hub.reading_counts(Some(&[self.ws]), &[]).unwrap();
        (counts.unread, counts.needs_reader)
    }

    fn recognize_requests(&self, hash: &str) -> usize {
        requests_for(&self.device, hash)
            .iter()
            .filter(|r| want_of(r) == koru_read::Want::Recognize)
            .count()
    }

    fn all_recognize_requests(&self) -> usize {
        self.files
            .iter()
            .map(|(h, ..)| self.recognize_requests(h))
            .sum()
    }

    /// Step the coordinator at one instant until it has nothing to do, as the
    /// daemon's loop does between two waits. A spin fails here. Returns the
    /// steps that did something.
    fn burst(&mut self) -> usize {
        let device = &mut self.device;
        let mut steps = 0;
        while device.coordinator.step(device.now) {
            device.applier.step();
            steps += 1;
            assert!(steps < 100, "the coordinator spun");
        }
        device.applier.step();
        steps
    }
}

/// **The big run's stall, reproduced, and what happens instead now.**
///
/// The reader is installed over a backlog; the first recognition job goes
/// silent and is ended (`crashed`); the next workers do not answer their
/// status for a while; then one is healthy. Before OCR Plan 3.6 every file
/// behind the hung one was taken off the plan as "skipped", because a failed
/// probe read as "the reader is gone": the plan emptied and the backlog was
/// called finished with six files counted unread, `needs_reader` 0 and
/// nothing queued — the run's numbers. The recount that followed, still
/// meeting a silent worker, counted them as needing the reader that was
/// installed, and for pictures nothing asked the worker again until some
/// listing changed.
///
/// Now they wait for the worker by the path a worker that is down already
/// takes; it is asked once per retry rather than once per file, and when it
/// answers every file is read.
fn a_worker_that_cannot_be_asked_holds_the_backlog_until_it_answers(kind: Kind) {
    const OUTAGE: usize = 2;
    let mut run = Waiting::new(kind, OUTAGE);

    run.hub.set_installed(Some(installed_model()));
    run.trouble.hang.store(true, Ordering::Relaxed);
    run.burst();
    let coordinator = &run.device.coordinator;
    assert_eq!(
        coordinator.stats.skipped,
        0,
        "a worker that cannot be asked is not a reader that is gone \
         (queued {}, waiting {}, unread and needs_reader {:?})",
        coordinator.queued().len(),
        coordinator.counts.waiting_for_reader,
        run.counts(),
    );
    assert_eq!(
        coordinator.counts.waiting_for_reader, 5,
        "every file behind the hung one waits for the worker"
    );
    assert!(matches!(
        coordinator.worker_down,
        Some(Unavailable::NotReady(_))
    ));
    assert_eq!(
        run.failing.load(Ordering::Relaxed),
        OUTAGE - 1,
        "the silent worker was asked once, not once per file"
    );
    assert_eq!(run.counts(), (6, 0));

    // The recount the crash asked for runs, and the worker is not asked again
    // before the retry: everything is queued for the reader, and waits.
    run.device.now += coordinator::RECOMPUTE_EVERY_MILLIS + 1;
    run.burst();
    assert_eq!(run.device.coordinator.counts.waiting_for_reader, 6);
    assert_eq!(run.failing.load(Ordering::Relaxed), OUTAGE - 1, "not asked");
    assert_eq!(
        run.counts(),
        (6, 0),
        "installed, and waiting for the worker: not `needs_reader`"
    );

    // The retry: still silent, asked once more, and everything waits again.
    run.device.now += coordinator::READER_RETRY_MILLIS;
    run.burst();
    assert_eq!(run.failing.load(Ordering::Relaxed), OUTAGE - 2);
    assert_eq!(run.device.coordinator.counts.waiting_for_reader, 6);

    // The next retry meets a healthy worker, and every file is read. The
    // hung one resumes past the page it hung on, as after any crash — for a
    // picture, its only page — so it is settled, and its word is not asked.
    run.device.now += coordinator::READER_RETRY_MILLIS;
    run.device.settle();
    let hung = &run.files[0];
    assert!(
        run.device
            .reading(run.ws, hung.1, &hung.0)
            .is_some_and(|r| r.is_settled_for_this_build()),
        "the hung file is settled"
    );
    for (h, document, word) in &run.files {
        if h != &hung.0 {
            assert!(run.device.found(word, *document), "{word} is found");
        }
        assert_eq!(
            run.recognize_requests(h),
            if *h == hung.0 { 2 } else { 1 },
            "read by the reader once, the hung one twice"
        );
    }
    assert_eq!(run.counts(), (0, 0));
    let coordinator = &run.device.coordinator;
    assert_eq!(coordinator.stats.skipped, 0);
    assert_eq!(coordinator.counts.waiting_for_reader, 0);
    assert_eq!(coordinator.worker_down, None);
}

#[test]
fn a_worker_that_cannot_be_asked_holds_a_backlog_of_scans() {
    a_worker_that_cannot_be_asked_holds_the_backlog_until_it_answers(Kind::Scans);
}

#[test]
fn a_worker_that_cannot_be_asked_holds_a_backlog_of_pictures() {
    a_worker_that_cannot_be_asked_holds_the_backlog_until_it_answers(Kind::Pictures);
}

/// **A worker that never answers is asked once per retry, never spun on**:
/// each pass over the queue ends at once with everything waiting, and only
/// the passing of [`coordinator::READER_RETRY_MILLIS`] asks it again.
#[test]
fn a_worker_that_never_answers_is_asked_once_per_retry() {
    let mut run = Waiting::new(Kind::Scans, usize::MAX);
    run.hub.set_installed(Some(installed_model()));
    run.trouble.hang.store(true, Ordering::Relaxed);
    run.burst();
    let asked = |run: &Waiting| usize::MAX - run.failing.load(Ordering::Relaxed);
    assert_eq!(asked(&run), 1);
    for retry in 1..=10 {
        run.device.now += coordinator::READER_RETRY_MILLIS / 2;
        run.burst();
        assert_eq!(asked(&run), retry, "not asked inside the retry");
        run.device.now += coordinator::READER_RETRY_MILLIS / 2;
        let steps = run.burst();
        assert_eq!(asked(&run), retry + 1, "asked once at the retry");
        assert!(steps <= WORDS.len() + 1, "{steps} steps in one pass");
    }
    assert_eq!(run.device.coordinator.stats.skipped, 0);
    assert_eq!(run.counts(), (6, 0));
    assert_eq!(run.all_recognize_requests(), 1, "only the hung job");
}

/// **A reader that is really gone still takes its pairs off the plan, and the
/// counts say they wait for one** — with no listing change or any other
/// trigger. Here the model would not load: this launch then answers
/// `recognition: false`, so every file queued for it is skipped, and the
/// recount counts all six as `needs_reader`. Nothing waits for the worker,
/// and nothing asks it again.
#[test]
fn a_reader_that_is_gone_takes_its_pairs_off_the_plan_and_counts_them() {
    let mut run = Waiting::new(Kind::Scans, 0);
    run.trouble.will_not_load.store(true, Ordering::Relaxed);
    run.hub.set_installed(Some(installed_model()));
    run.burst();
    let coordinator = &run.device.coordinator;
    assert_eq!(coordinator.stats.skipped, 5, "off the plan");
    assert!(coordinator.queued().is_empty());
    assert_eq!(coordinator.counts.waiting_for_reader, 0);
    assert_eq!(coordinator.worker_down, None);

    run.device.now += coordinator::RECOMPUTE_EVERY_MILLIS + 1;
    run.burst();
    assert_eq!(run.counts(), (6, 6), "counted as waiting for a reader");
    assert!(run.device.coordinator.queued().is_empty());
    for _ in 0..5 {
        run.device.now += coordinator::READER_RETRY_MILLIS;
        run.burst();
    }
    assert_eq!(run.all_recognize_requests(), 1, "not asked again");
    assert_eq!(run.counts(), (6, 6));
}

/// **Counts that lose an item are counted again**: when the plan empties with
/// more unread than the waiting categories hold, the coordinator asks for a
/// recompute instead of going idle — and an ordinary empty plan does not.
/// The loss is made by hand here (the queue's counts emptied), since no path
/// is known to make it any more.
#[test]
fn counts_that_lose_an_item_are_counted_again() {
    let picture = hash(80);
    let mut store = store();
    let ws = workspace(&mut store);
    a_page_with_picture(&mut store, ws, &picture);
    land(&mut store, ws, &picture, 10, "image/png");
    let mut device = Device::new(store, stub(library(&[])));
    device.here(&picture);
    device.settle();
    assert_eq!(device.coordinator.counts.needs_reader, 1);

    device.now += coordinator::RECOMPUTE_EVERY_MILLIS + 1;
    let recomputes = device.coordinator.stats.recomputes;
    assert!(!device.coordinator.step(device.now), "ordinary: idle");
    assert_eq!(device.coordinator.stats.recomputes, recomputes);

    device.coordinator.counts = coordinator::QueueCounts::default();
    assert!(
        device.coordinator.step(device.now),
        "an unread item nothing holds: step again"
    );
    assert!(!device.coordinator.step(device.now), "counted again, idle");
    assert_eq!(device.coordinator.stats.recomputes, recomputes + 1);
    assert_eq!(device.coordinator.counts.needs_reader, 1);
}
