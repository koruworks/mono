// SPDX-License-Identifier: MPL-2.0

//! **One `Read`, from a path to its last message.**
//!
//! The job opens the path read-only, decides what the bytes are, and reads a
//! PDF page by page, decodes a plain-text file, recognizes a picture, or says
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
//! | a page whose text layer fails the acceptance check, no reader | that page empty, and the job `partial` with no reason: it waits for the reader |
//! | the same page, with the reader | the page rendered and recognized: `recognized`, with a confidence |
//! | a page with a usable layer and pictures over half of it, with the reader | both, combined ([`crate::mixed`]): `mixed`, with the recognized part's confidence |
//! | a page that would be the job's `max_read_pages + 1`-th recognized | the job stops before it: `partial/page-cap`, `pages_done` the page before |
//! | more pages than `max_text_pages` | `partial/page-cap` |
//! | words on any page | `read` |
//! | no words anywhere | `empty` |
//! | every page abandoned at its budget or panicking | `Failed internal` — a local fact, retried elsewhere |
//! | the model would not load | `Failed needs-reader` |
//!
//! "Waits for the reader" wins over the text cap: a reading that still needs
//! recognition is not finished, and when the reader is installed it is read
//! again and capped then. **Text-layer pages never count against the reading
//! cap**; a page recognized as well as its layer does, because it was sent to
//! the reader.
//!
//! ## What a picture job answers
//!
//! | Found | Answer |
//! | --- | --- |
//! | asked for text only | `Failed needs-reader`, as before |
//! | no reader here | `Failed needs-reader` (a build with recognition, no model) or `Failed unsupported` (a build without) |
//! | not PNG, JPEG, GIF, WebP, BMP or TIFF, or past the size bounds | `Failed unsupported` |
//! | one of those whose bytes do not decode | `Done failed/damaged` |
//! | words | page 1, `recognized`, then `Done read` |
//! | none | page 1 empty, then `Done empty` |

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use koru_read::{
    Event, FailureCode, JobId, PageText, ReadingStatus, Reason, Recognized, Source, Want,
};

use crate::accept::{Ask, Layer, Verdict, ask, judge};
use crate::budget::{self, Abandoned, Outcome, Waited};
use crate::mixed;
use crate::pdf::{self, Document};
use crate::picture::{self, DecodeError};
use crate::plain;
use crate::recognition::Recognition;

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
    /// The reader, or why there is none.
    pub recognition: Arc<Recognition>,
}

impl Engine {
    /// The worker's own: the disk, `hayro`, the product budgets — and no
    /// recognition until [`Engine::with_recognition`] gives it some.
    pub fn product(abandoned: Arc<Abandoned>) -> Engine {
        Engine {
            files: Arc::new(Disk),
            open_pdf: Arc::new(|bytes| {
                pdf::open(bytes).map(|document| Arc::new(document) as Arc<dyn Document>)
            }),
            page_budget: budget::PAGE_BUDGET,
            load_budget: budget::LOAD_BUDGET,
            abandoned,
            recognition: Arc::new(Recognition::none(koru_read_paddle::RECOGNITION_BUILT)),
        }
    }

    pub fn with_recognition(mut self, recognition: Recognition) -> Engine {
        self.recognition = Arc::new(recognition);
        self
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
    /// At most this many pages recognized; `None` for every page.
    pub max_read_pages: Option<u32>,
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
    /// A picture: recognition's.
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
        // reader's absence, and nothing more is read.
        Kind::Picture => match job.want {
            Want::Text => failed(FailureCode::NeedsReader),
            Want::Recognize if !engine.recognition.available() => {
                failed(engine.recognition.absent())
            }
            Want::Recognize => {
                let rest = picture::MAX_PICTURE_FILE_BYTES.saturating_sub(bytes.len() as u64);
                if file.take(rest + 1).read_to_end(&mut bytes).is_err() {
                    return failed(FailureCode::Unreadable);
                }
                if bytes.len() as u64 > picture::MAX_PICTURE_FILE_BYTES {
                    return failed(FailureCode::Unsupported);
                }
                read_picture(engine, job, bytes, cancel, emit)
            }
        },
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

/// How one picture's read went, inside its budget.
type PictureRead = Result<Result<Recognized, FailureCode>, DecodeError>;

fn read_picture(
    engine: &Engine,
    job: &ReadJob,
    bytes: Vec<u8>,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event) -> bool,
) -> Event {
    let id = job.job;
    let failed = |code| Event::Failed { job: id, code };
    let reader = match engine.recognition.take() {
        Ok(reader) => reader,
        Err(code) => return failed(code),
    };
    // Decoding is untrusted work too, so it runs inside the page budget with
    // the read.
    let outcome = budget::bounded(
        "koru-read-picture",
        engine.page_budget,
        cancel,
        &engine.abandoned,
        move || {
            let mut reader = reader;
            let result: PictureRead = picture::decode(&bytes).and_then(|pixels| {
                let picture = pixels.picture().ok_or(DecodeError::Damaged)?;
                Ok(reader.read(&picture).map_err(|error| error.code))
            });
            (reader, result)
        },
    );
    let result = match outcome {
        Outcome::Done((reader, result)) => {
            engine.recognition.give_back(reader);
            result
        }
        Outcome::TimedOut => {
            tracing::warn!(job = %id, "a picture ran past its time budget and was abandoned");
            return failed(FailureCode::Internal);
        }
        Outcome::Failed => return failed(FailureCode::Internal),
        Outcome::Cancelled => return failed(FailureCode::Cancelled),
    };
    let recognized = match result {
        Ok(Ok(recognized)) => recognized,
        Ok(Err(code)) => return failed(code),
        Err(DecodeError::Damaged) => {
            return Event::Done {
                job: id,
                status: ReadingStatus::Failed,
                reason: Some(Reason::Damaged),
                pages_done: 0,
            };
        }
        Err(DecodeError::Unsupported | DecodeError::TooLarge) => {
            return failed(FailureCode::Unsupported);
        }
    };
    let words = !recognized.text.as_str().trim().is_empty();
    let event = Event::Page {
        job: id,
        page: 1,
        text: recognized.text,
        source: Source::Recognized,
        confidence: Some(recognized.confidence),
    };
    if !emit(event) {
        return failed(FailureCode::Cancelled);
    }
    Event::Done {
        job: id,
        status: if words {
            ReadingStatus::Read
        } else {
            ReadingStatus::Empty
        },
        reason: None,
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
    /// Pages sent to the reader, against `max_read_pages`.
    recognized: u32,
}

/// How recognizing one page went.
enum Step {
    Read(Recognized),
    /// Rendering or reading failed, panicked or ran past the budget; or the
    /// page's images are too large to render.
    Lost,
    Cancelled,
    /// The model would not load.
    NoReader(FailureCode),
}

/// Render page `number` and read it, under the page budget.
fn recognize_page(
    engine: &Engine,
    document: &Arc<dyn Document>,
    number: u32,
    layer: &Layer,
    job: JobId,
    cancel: &AtomicBool,
) -> Step {
    if !picture::page_images_allowed(layer) {
        // Rendering decodes every image at full size; this page's declared
        // sizes would make that larger than the worker may be.
        tracing::warn!(
            job = %job,
            page = number,
            "a page draws images too large to render and was not recognized"
        );
        return Step::Lost;
    }
    let reader = match engine.recognition.take() {
        Ok(reader) => reader,
        Err(code) => return Step::NoReader(code),
    };
    let document = Arc::clone(document);
    let outcome = budget::bounded(
        "koru-read-render",
        engine.page_budget,
        cancel,
        &engine.abandoned,
        move || {
            let mut reader = reader;
            let result = document.render(number).and_then(|pixels| {
                let picture = pixels.picture()?;
                reader.read(&picture).ok()
            });
            (reader, result)
        },
    );
    match outcome {
        Outcome::Done((reader, result)) => {
            engine.recognition.give_back(reader);
            match result {
                Some(recognized) => Step::Read(recognized),
                None => Step::Lost,
            }
        }
        Outcome::TimedOut => {
            tracing::warn!(
                job = %job,
                page = number,
                "recognizing a page ran past its time budget and was abandoned"
            );
            Step::Lost
        }
        Outcome::Failed => Step::Lost,
        Outcome::Cancelled => Step::Cancelled,
    }
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

    // Recognition only when it was asked for and is here: a text job never
    // takes the reader, so it never loads the model.
    let recognize = job.want == Want::Recognize && engine.recognition.available();
    let count = document.page_count();
    let first = job.from_page.max(1);
    let last = match job.max_text_pages {
        Some(cap) => count.min((first - 1).saturating_add(cap)),
        None => count,
    };
    let capped = last < count;
    let mut tally = Tally::default();
    // Where the reading cap stopped the job: the last page it finished.
    let mut stopped_at: Option<u32> = None;
    let mut next = first;
    // One reader thread at a time, reading on from `next`. A page that runs
    // past its budget, or panics, costs that reader; the next one starts
    // after the lost page.
    'pages: while next <= last {
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
                    let verdict = judge(&layer);
                    let wanted = if recognize {
                        ask(&layer, verdict)
                    } else {
                        Ask::Nothing
                    };
                    let step = if wanted == Ask::Nothing {
                        None
                    } else if job
                        .max_read_pages
                        .is_some_and(|cap| tally.recognized >= cap)
                    {
                        // **The reading cap.** This page would be one more
                        // than the job may send to the reader: stop before
                        // it, so a resume at `pages_done + 1` starts here.
                        stop.store(true, Ordering::Release);
                        tally.examined -= 1;
                        stopped_at = Some(number - 1);
                        break 'pages;
                    } else {
                        tally.recognized += 1;
                        Some(recognize_page(
                            engine, &document, number, &layer, id, cancel,
                        ))
                    };
                    let event = match (verdict, step) {
                        (_, Some(Step::Cancelled)) => {
                            stop.store(true, Ordering::Release);
                            return failed(FailureCode::Cancelled);
                        }
                        (_, Some(Step::NoReader(code))) => {
                            stop.store(true, Ordering::Release);
                            return failed(code);
                        }
                        // Recognized instead of an unusable layer.
                        (_, Some(Step::Read(recognized))) if wanted == Ask::Instead => {
                            if !recognized.text.as_str().trim().is_empty() {
                                tally.with_words += 1;
                            }
                            Event::Page {
                                job: id,
                                page: number,
                                text: recognized.text,
                                source: Source::Recognized,
                                confidence: Some(recognized.confidence),
                            }
                        }
                        // Recognized as well as a real layer: both, once.
                        (_, Some(Step::Read(recognized))) => {
                            tally.with_words += 1;
                            Event::Page {
                                job: id,
                                page: number,
                                text: PageText::new(mixed::combine(
                                    &layer.text,
                                    recognized.text.as_str(),
                                )),
                                source: Source::Mixed,
                                confidence: Some(recognized.confidence),
                            }
                        }
                        // The page could not be recognized: an unusable
                        // layer gives nothing, a real one is kept.
                        (_, Some(Step::Lost)) if wanted == Ask::Instead => {
                            tally.lost += 1;
                            page(id, number, String::new(), Source::Recognized)
                        }
                        (_, Some(Step::Lost)) | (Verdict::Text, None) => {
                            if !layer.text.is_empty() {
                                tally.with_words += 1;
                            }
                            page(id, number, layer.text, Source::TextLayer)
                        }
                        (Verdict::Nothing, None) => {
                            page(id, number, String::new(), Source::TextLayer)
                        }
                        (Verdict::NeedsReader, None) => {
                            tally.needs_reader += 1;
                            page(id, number, String::new(), Source::TextLayer)
                        }
                    };
                    if !emit(event) {
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
    let (status, reason) = if stopped_at.is_some() {
        (ReadingStatus::Partial, Some(Reason::PageCap))
    } else if tally.needs_reader > 0 {
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
        recognized = tally.recognized,
        lost = tally.lost,
        status = status.as_str(),
        "a PDF was read"
    );
    // How far the reading has got: where the reading cap stopped it, the
    // last page this job reached, or, when it reached none (a resume past the
    // end, or a cap of nothing), where it would have started.
    let pages_done = match stopped_at {
        Some(page) => page.max(first - 1),
        None if tally.examined > 0 => last,
        None => count.min(first - 1),
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
