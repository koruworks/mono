// SPDX-License-Identifier: MPL-2.0

//! **One `Read`, from a path to its last message.**
//!
//! The job opens the path read-only, decides what the bytes are, and either
//! reads a PDF's text layers page by page, decodes a plain-text file, or says
//! why it cannot. It speaks only through `emit` and its return value, and the
//! rules it keeps are the contract's (`koru_read::wire`):
//!
//! - **every page in the range gets exactly one `Page`, in order** — the
//!   page's words, or empty text as progress when the page had none, needs
//!   the reader, or was abandoned — so the coordinator always knows how far
//!   the job got;
//! - **the job ends with exactly one terminal event**, which `run` returns
//!   rather than emits, so the caller can clear its state before sending it
//!   and a `Read` that follows the `Done` at once is never answered `Busy`.
//!
//! ## What a PDF job answers
//!
//! | Found | Answer |
//! | --- | --- |
//! | a user password, or a certificate lock | `Done failed/password` |
//! | nothing a parser can make a document of | `Done failed/damaged` |
//! | a page whose text layer fails the acceptance check | that page empty, and the job `partial` with no reason: it waits for the reader |
//! | more pages than `max_text_pages` | `partial/page-cap` |
//! | words on any page | `read` |
//! | no words anywhere | `empty` |
//! | every page abandoned at its budget or panicking | `Failed internal` — a local fact, retried elsewhere |
//!
//! "Waits for the reader" wins over "page cap": a reading that still needs
//! recognition is not finished, and when the reader is installed it is read
//! again and capped then.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use koru_read::{Event, FailureCode, JobId, PageText, ReadingStatus, Reason, Source, Want};

use crate::accept::{Layer, Verdict, judge};
use crate::budget::{self, Abandoned, Outcome, Waited};
use crate::pdf::{self, Document};
use crate::plain;

/// How much of a file is looked at to decide what it is.
const SNIFF_BYTES: u64 = 1024;

/// **Where files come from.** The worker's own is the disk, read-only; a test
/// hands bytes from memory, so no test touches a disk.
pub trait Files: Send + Sync {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Read + Send>>;
}

/// The disk. Opened for reading only, and only a regular file.
pub struct Disk;

impl Files for Disk {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Read + Send>> {
        let file = std::fs::OpenOptions::new().read(true).open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        Ok(Box::new(file))
    }
}

/// Opens a PDF's bytes as a document. The worker's own is `hayro`; a test can
/// stand in a document whose page never finishes.
pub type PdfOpener = dyn Fn(Vec<u8>) -> Result<Arc<dyn Document>, Reason> + Send + Sync;

/// Everything a job needs that is not the request.
#[derive(Clone)]
pub struct Engine {
    pub files: Arc<dyn Files>,
    pub open_pdf: Arc<PdfOpener>,
    pub page_budget: Duration,
    pub load_budget: Duration,
    pub abandoned: Arc<Abandoned>,
}

impl Engine {
    /// The worker's own: the disk, `hayro`, the product budgets.
    pub fn product(abandoned: Arc<Abandoned>) -> Engine {
        Engine {
            files: Arc::new(Disk),
            open_pdf: Arc::new(|bytes| {
                pdf::open(bytes).map(|document| Arc::new(document) as Arc<dyn Document>)
            }),
            page_budget: budget::PAGE_BUDGET,
            load_budget: budget::LOAD_BUDGET,
            abandoned,
        }
    }
}

/// The fields of one `Request::Read`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadJob {
    pub job: JobId,
    pub path: PathBuf,
    pub mime: String,
    pub from_page: u32,
    pub max_text_pages: Option<u32>,
    pub want: Want,
}

/// What a file is, as far as this worker is concerned.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Pdf,
    /// Plain text; `true` for HTML, whose markup is stripped.
    Plain {
        html: bool,
    },
    /// A picture: recognition's, which this build does not have.
    Picture,
    Unknown,
}

/// Decide from the recorded MIME type and the first bytes. **The bytes win**
/// where they say PDF: a PDF stored with the wrong type is still a PDF.
pub fn classify(mime: &str, head: &[u8]) -> Kind {
    if head.windows(5).any(|window| window == b"%PDF-") {
        return Kind::Pdf;
    }
    let essence = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "application/pdf" | "application/x-pdf" => Kind::Pdf,
        "text/html" | "application/xhtml+xml" => Kind::Plain { html: true },
        "text/plain" | "text/markdown" | "text/x-markdown" | "text/csv" => {
            Kind::Plain { html: false }
        }
        other if other.starts_with("image/") => Kind::Picture,
        _ => Kind::Unknown,
    }
}

/// Run one job to its end. Pages go to `emit` as they are read (`emit`
/// answering `false` means nobody is listening, which ends the job as
/// cancelled); the terminal event is returned.
pub fn run(
    engine: &Engine,
    job: &ReadJob,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event) -> bool,
) -> Event {
    let failed = |code| Event::Failed { job: job.job, code };
    let Ok(mut file) = engine.files.open(&job.path) else {
        return failed(FailureCode::Unreadable);
    };
    let mut bytes = Vec::new();
    if (&mut file)
        .take(SNIFF_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failed(FailureCode::Unreadable);
    }
    match classify(&job.mime, &bytes) {
        Kind::Pdf => {
            if file.read_to_end(&mut bytes).is_err() {
                return failed(FailureCode::Unreadable);
            }
            drop(file);
            read_pdf(engine, job, bytes, cancel, emit)
        }
        Kind::Plain { html } => {
            let rest = plain::PLAIN_FILE_MAX_BYTES.saturating_sub(bytes.len() as u64);
            if file.take(rest + 1).read_to_end(&mut bytes).is_err() {
                return failed(FailureCode::Unreadable);
            }
            read_plain(job, bytes, html, cancel, emit)
        }
        // A picture needs recognition. Asked for text only, that is the
        // reader's absence; asked to recognize, it is this build's.
        Kind::Picture => failed(match job.want {
            Want::Text => FailureCode::NeedsReader,
            Want::Recognize => FailureCode::Unsupported,
        }),
        Kind::Unknown => failed(FailureCode::Unsupported),
    }
}

fn page(job: JobId, number: u32, text: String, source: Source) -> Event {
    Event::Page {
        job,
        page: number,
        text: PageText::new(text),
        source,
        confidence: None,
    }
}

fn read_plain(
    job: &ReadJob,
    bytes: Vec<u8>,
    html: bool,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event) -> bool,
) -> Event {
    let cut_on_disk = bytes.len() as u64 > plain::PLAIN_FILE_MAX_BYTES;
    let bytes = if cut_on_disk {
        &bytes[..plain::PLAIN_FILE_MAX_BYTES as usize]
    } else {
        &bytes[..]
    };
    let mut text = plain::decode(bytes);
    if html {
        text = plain::strip_html(&text);
    }
    let (text, cut) = plain::truncate(text, plain::PLAIN_TEXT_MAX_BYTES);
    let capped = cut || cut_on_disk;
    if cancel.load(Ordering::Acquire) {
        return Event::Failed {
            job: job.job,
            code: FailureCode::Cancelled,
        };
    }
    let words = !text.trim().is_empty();
    // A plain file is one page, whatever `from_page` says.
    if !emit(page(job.job, 1, text, Source::Plain)) {
        return Event::Failed {
            job: job.job,
            code: FailureCode::Cancelled,
        };
    }
    let (status, reason) = match (capped, words) {
        (true, _) => (ReadingStatus::Partial, Some(Reason::PageCap)),
        (false, true) => (ReadingStatus::Read, None),
        (false, false) => (ReadingStatus::Empty, None),
    };
    Event::Done {
        job: job.job,
        status,
        reason,
        pages_done: 1,
    }
}

/// How the pages of one PDF job went.
#[derive(Default)]
struct Tally {
    examined: u32,
    with_words: u32,
    needs_reader: u32,
    /// Abandoned at the page budget, or the page panicked.
    lost: u32,
}

fn read_pdf(
    engine: &Engine,
    job: &ReadJob,
    bytes: Vec<u8>,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event) -> bool,
) -> Event {
    let id = job.job;
    let failed = |code| Event::Failed { job: id, code };
    let opener = Arc::clone(&engine.open_pdf);
    let document = match budget::bounded(
        "koru-read-open",
        engine.load_budget,
        cancel,
        &engine.abandoned,
        move || opener(bytes),
    ) {
        Outcome::Done(Ok(document)) => document,
        Outcome::Done(Err(reason)) => {
            return Event::Done {
                job: id,
                status: ReadingStatus::Failed,
                reason: Some(reason),
                pages_done: 0,
            };
        }
        Outcome::TimedOut => {
            tracing::warn!(job = %id, "opening a file exceeded its time budget and was abandoned");
            return failed(FailureCode::Internal);
        }
        Outcome::Failed => return failed(FailureCode::Internal),
        Outcome::Cancelled => return failed(FailureCode::Cancelled),
    };

    let count = document.page_count();
    let first = job.from_page.max(1);
    let last = match job.max_text_pages {
        Some(cap) => count.min((first - 1).saturating_add(cap)),
        None => count,
    };
    let capped = last < count;
    let mut tally = Tally::default();
    let mut next = first;
    // One reader thread at a time, reading on from `next`. A page that runs
    // past its budget, or panics, costs that reader; the next one starts
    // after the lost page.
    while next <= last {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(u32, Layer)>(0);
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let reader = {
            let document = Arc::clone(&document);
            let stop = Arc::clone(&stop);
            let finished = Arc::clone(&finished);
            let from = next;
            std::thread::Builder::new()
                .name("koru-read-pages".into())
                .stack_size(budget::WORK_STACK)
                .spawn(move || {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        document.read_pages(from, last, &mut |number, layer| {
                            !stop.load(Ordering::Acquire) && tx.send((number, layer)).is_ok()
                        });
                    }));
                    finished.store(true, Ordering::Release);
                })
        };
        if reader.is_err() {
            return failed(FailureCode::Internal);
        }
        loop {
            match budget::wait(&rx, engine.page_budget, cancel) {
                Waited::Got((number, layer)) if number == next => {
                    tally.examined += 1;
                    let text = match judge(&layer) {
                        Verdict::Text => {
                            if !layer.text.is_empty() {
                                tally.with_words += 1;
                            }
                            layer.text
                        }
                        Verdict::Nothing => String::new(),
                        Verdict::NeedsReader => {
                            tally.needs_reader += 1;
                            String::new()
                        }
                    };
                    if !emit(page(id, number, text, Source::TextLayer)) {
                        stop.store(true, Ordering::Release);
                        return failed(FailureCode::Cancelled);
                    }
                    next += 1;
                    if next > last {
                        break;
                    }
                }
                // The page ran past its budget (the reader is abandoned and
                // told to stop at its next chance), the reader ended without
                // it (the page panicked), or it answered a page it was not
                // asked for (a bug in a `Document`): the page is lost and a
                // fresh reader starts after it.
                lost @ (Waited::Got(_) | Waited::Ended | Waited::TimedOut) => {
                    stop.store(true, Ordering::Release);
                    engine.abandoned.note(Arc::clone(&finished));
                    // A line about one page is news here: it is rare, and it
                    // is what explains a page missing from a reading.
                    if matches!(lost, Waited::TimedOut) {
                        tracing::warn!(
                            job = %id,
                            page = next,
                            "a page ran past its time budget and was abandoned"
                        );
                    } else {
                        tracing::warn!(job = %id, page = next, "a page could not be read");
                    }
                    tally.examined += 1;
                    tally.lost += 1;
                    if !emit(page(id, next, String::new(), Source::TextLayer)) {
                        return failed(FailureCode::Cancelled);
                    }
                    next += 1;
                    break;
                }
                Waited::Cancelled => {
                    stop.store(true, Ordering::Release);
                    if !finished.load(Ordering::Acquire) {
                        engine.abandoned.note(Arc::clone(&finished));
                    }
                    return failed(FailureCode::Cancelled);
                }
            }
        }
    }

    if tally.examined > 0 && tally.lost == tally.examined {
        return failed(FailureCode::Internal);
    }
    let (status, reason) = if tally.needs_reader > 0 {
        (ReadingStatus::Partial, None)
    } else if capped {
        (ReadingStatus::Partial, Some(Reason::PageCap))
    } else if tally.with_words > 0 {
        (ReadingStatus::Read, None)
    } else {
        (ReadingStatus::Empty, None)
    };
    tracing::debug!(
        job = %id,
        pages = tally.examined,
        with_words = tally.with_words,
        needs_reader = tally.needs_reader,
        lost = tally.lost,
        status = status.as_str(),
        "a PDF was read"
    );
    // How far the reading has got: the last page this job reached, or, when
    // it reached none (a resume past the end, or a cap of nothing), where it
    // would have started.
    let pages_done = if tally.examined > 0 {
        last
    } else {
        count.min(first - 1)
    };
    Event::Done {
        job: id,
        status,
        reason,
        pages_done,
    }
}

#[cfg(test)]
mod tests;
