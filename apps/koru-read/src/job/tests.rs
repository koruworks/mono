// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::{self, Memory, Script, stuck};
use std::time::Instant;

const JOB: JobId = JobId(9);

/// An `Abandoned` that never ends the test process.
fn lenient() -> Arc<Abandoned> {
    Abandoned::new(64, Duration::from_secs(3600), |_| {})
}

fn engine(files: Memory) -> Engine {
    Engine {
        files: Arc::new(files),
        page_budget: Duration::from_secs(30),
        load_budget: Duration::from_secs(30),
        ..Engine::product(lenient())
    }
}

fn request(path: &str, mime: &str) -> ReadJob {
    ReadJob {
        job: JOB,
        path: PathBuf::from(path),
        mime: mime.into(),
        from_page: 1,
        max_text_pages: Some(koru_read::TEXT_PAGE_CAP),
        max_read_pages: Some(koru_read::READ_PAGE_CAP),
        want: Want::Text,
    }
}

/// Run a job; answer its pages as `(number, text, source)` and its end.
fn read(engine: &Engine, job: &ReadJob) -> (Vec<(u32, String, Source)>, Event) {
    let cancel = AtomicBool::new(false);
    let mut pages = Vec::new();
    let end = run(engine, job, &cancel, &mut |event| {
        match event {
            Event::Page {
                job: id,
                page,
                text,
                source,
                confidence,
            } => {
                assert_eq!(id, JOB);
                assert_eq!(confidence, None, "text has no confidence");
                pages.push((page, text.into_string(), source));
            }
            other => panic!("only pages are emitted; the end is returned: {other:?}"),
        }
        true
    });
    (pages, end)
}

fn pdf(bytes: Vec<u8>) -> (Vec<(u32, String, Source)>, Event) {
    read(
        &engine(Memory::default().with("objects/b3-pdf", bytes)),
        &request("objects/b3-pdf", "application/pdf"),
    )
}

fn done(status: ReadingStatus, reason: Option<Reason>, pages_done: u32) -> Event {
    Event::Done {
        job: JOB,
        status,
        reason,
        pages_done,
    }
}

fn failed(code: FailureCode) -> Event {
    Event::Failed { job: JOB, code }
}

fn text_page(number: u32, text: &str) -> (u32, String, Source) {
    (number, text.to_string(), Source::TextLayer)
}

// ------------------------------------------------------- the fixtures, whole

#[test]
fn a_text_pdf_reads_to_its_words() {
    let (pages, end) = pdf(fixtures::one_page());
    assert_eq!(pages, [text_page(1, fixtures::ONE_PAGE_TEXT)]);
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
}

#[test]
fn a_two_page_pdf_reads_page_by_page() {
    let (pages, end) = pdf(fixtures::two_pages());
    assert_eq!(
        pages,
        [
            text_page(1, "alpha bravo charlie"),
            text_page(2, "delta echo foxtrot")
        ]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 2));
}

#[test]
fn a_password_protected_pdf_ends_failed_password() {
    let (pages, end) = pdf(fixtures::encrypted());
    assert!(pages.is_empty());
    assert_eq!(end, done(ReadingStatus::Failed, Some(Reason::Password), 0));
}

#[test]
fn a_permissions_only_pdf_reads() {
    let (pages, end) = pdf(fixtures::permissions_only());
    assert_eq!(pages, [text_page(1, "unlocked words appear here")]);
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
}

#[test]
fn a_damaged_pdf_ends_failed_damaged() {
    let (pages, end) = pdf(fixtures::damaged());
    assert!(pages.is_empty());
    assert_eq!(end, done(ReadingStatus::Failed, Some(Reason::Damaged), 0));
}

/// **No model, so a scanned page waits for the reader**: the typed page is
/// delivered, the scan is progress with no words, and the job is `partial`
/// with no reason — unfinished, not capped.
#[test]
fn a_pdf_with_a_scanned_page_is_partial_and_waits_for_the_reader() {
    let (pages, end) = pdf(fixtures::text_then_scan());
    assert_eq!(
        pages,
        [
            text_page(1, "typed words on the first page"),
            text_page(2, "")
        ]
    );
    assert_eq!(end, done(ReadingStatus::Partial, None, 2));
    let (_, end) = pdf(fixtures::unmapped());
    assert_eq!(end, done(ReadingStatus::Partial, None, 1));
}

#[test]
fn a_blank_page_does_not_hold_a_text_pdf_back() {
    let (pages, end) = pdf(fixtures::with_blank_page());
    assert_eq!(pages[1], text_page(2, ""));
    assert_eq!(end, done(ReadingStatus::Read, None, 3));
}

/// **The text cap**, and resuming after it.
#[test]
fn the_text_cap_stops_the_job_partial_and_a_resume_carries_on() {
    let files = Memory::default().with("objects/b3-long", fixtures::many_pages(5));
    let engine = engine(files);
    let mut job = request("objects/b3-long", "application/pdf");
    job.max_text_pages = Some(2);
    let (pages, end) = read(&engine, &job);
    assert_eq!(pages.iter().map(|p| p.0).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 2));

    job.from_page = 3;
    let (pages, end) = read(&engine, &job);
    assert_eq!(
        pages,
        [text_page(3, "page number 3"), text_page(4, "page number 4")]
    );
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 4));

    job.from_page = 5;
    let (pages, end) = read(&engine, &job);
    assert_eq!(pages, [text_page(5, "page number 5")]);
    assert_eq!(end, done(ReadingStatus::Read, None, 5));

    // *Read all pages*: no cap.
    job.from_page = 1;
    job.max_text_pages = None;
    let (pages, end) = read(&engine, &job);
    assert_eq!(pages.len(), 5);
    assert_eq!(end, done(ReadingStatus::Read, None, 5));

    // A resume past the end reads nothing and says how far the file goes.
    job.from_page = 9;
    let (pages, end) = read(&engine, &job);
    assert!(pages.is_empty());
    assert_eq!(end, done(ReadingStatus::Empty, None, 5));
}

#[test]
fn the_product_cap_is_the_contracts() {
    assert_eq!(request("p", "application/pdf").max_text_pages, Some(2_000));
}

#[test]
fn an_html_file_reads_without_its_markup() {
    let files = Memory::default().with("objects/b3-html", fixtures::HTML);
    let (pages, end) = read(&engine(files), &request("objects/b3-html", "text/html"));
    assert_eq!(pages, [(1, fixtures::HTML_TEXT.to_string(), Source::Plain)]);
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
}

#[test]
fn plain_text_files_read_as_they_are() {
    for mime in ["text/plain", "text/markdown", "text/csv; charset=utf-8"] {
        let files = Memory::default().with("objects/b3-txt", "# Title\r\nname,value\r\n");
        let (pages, end) = read(&engine(files), &request("objects/b3-txt", mime));
        assert_eq!(
            pages,
            [(1, "# Title\nname,value\n".to_string(), Source::Plain)],
            "{mime}"
        );
        assert_eq!(end, done(ReadingStatus::Read, None, 1));
    }
    let files = Memory::default().with("objects/b3-empty", "  \n");
    let (_, end) = read(&engine(files), &request("objects/b3-empty", "text/plain"));
    assert_eq!(end, done(ReadingStatus::Empty, None, 1));
}

#[test]
fn a_plain_file_past_its_cap_is_cut_and_partial() {
    let line = "the same sentence again and again\n";
    let big = line.repeat(plain::PLAIN_TEXT_MAX_BYTES / line.len() + 10);
    let files = Memory::default().with("objects/b3-big", big.clone());
    let (pages, end) = read(&engine(files), &request("objects/b3-big", "text/plain"));
    let kept = &pages[0].1;
    assert!(kept.len() <= plain::PLAIN_TEXT_MAX_BYTES);
    assert!(big.starts_with(kept.as_str()));
    assert!(
        big.as_bytes()[kept.len()].is_ascii_whitespace(),
        "cut at a word boundary, not inside a word"
    );
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 1));
}

/// The bytes win over the recorded type where they say PDF.
#[test]
fn a_pdf_stored_under_another_type_is_still_read_as_a_pdf() {
    let files = Memory::default().with("objects/b3-x", fixtures::one_page());
    let (pages, _) = read(
        &engine(files),
        &request("objects/b3-x", "application/octet-stream"),
    );
    assert_eq!(pages, [text_page(1, fixtures::ONE_PAGE_TEXT)]);
}

#[test]
fn a_picture_needs_the_reader_and_an_unknown_kind_is_unsupported() {
    let files = Memory::default()
        .with("objects/b3-png", b"\x89PNG\r\n\x1a\nrest".to_vec())
        .with("objects/b3-zip", b"PK\x03\x04rest".to_vec());
    let engine = engine(files);
    let (pages, end) = read(&engine, &request("objects/b3-png", "image/png"));
    assert!(pages.is_empty());
    assert_eq!(end, failed(FailureCode::NeedsReader));
    // Asked to recognize, with no reader here: a build with recognition
    // waits for one, a build without cannot.
    let mut recognize = request("objects/b3-png", "image/png");
    recognize.want = Want::Recognize;
    assert_eq!(
        read(&engine, &recognize).1,
        failed(if koru_read_paddle::RECOGNITION_BUILT {
            FailureCode::NeedsReader
        } else {
            FailureCode::Unsupported
        })
    );
    assert_eq!(
        read(&engine, &request("objects/b3-zip", "application/zip")).1,
        failed(FailureCode::Unsupported)
    );
}

#[test]
fn a_path_that_cannot_be_opened_is_unreadable() {
    let (pages, end) = read(
        &engine(Memory::default()),
        &request("objects/b3-gone", "application/pdf"),
    );
    assert!(pages.is_empty());
    assert_eq!(end, failed(FailureCode::Unreadable));
}

#[test]
fn classification_reads_the_essence_of_the_type() {
    assert_eq!(
        classify("Text/HTML; charset=UTF-8", b"<html>"),
        Kind::Plain { html: true }
    );
    assert_eq!(classify("text/markdown", b"#"), Kind::Plain { html: false });
    assert_eq!(classify("image/jpeg", b"\xFF\xD8"), Kind::Picture);
    assert_eq!(classify("image/jpeg", b"junk%PDF-1.7"), Kind::Pdf);
    assert_eq!(classify("application/json", b"{}"), Kind::Unknown);
}

// --------------------------------------------- pages that do not cooperate

fn scripted(pages: Vec<Script>, page_budget: Duration, abandoned: Arc<Abandoned>) -> Engine {
    fixtures::scripted_engine(pages, "objects/b3-s", page_budget, abandoned)
}

/// **A page that never finishes is abandoned at its budget**, and the job
/// reports a local failure, keeping only pages that actually finished.
#[test]
fn reading_diagnostics_a_page_that_never_finishes_is_abandoned_at_its_budget() {
    let (release, page_two) = stuck();
    let abandoned = lenient();
    let budget = Duration::from_millis(200);
    let engine = scripted(
        vec![
            Script::Words("first page words"),
            page_two,
            Script::Words("third page words"),
        ],
        budget,
        Arc::clone(&abandoned),
    );
    let started = Instant::now();
    let (pages, end) = read(&engine, &request("objects/b3-s", "application/pdf"));
    let took = started.elapsed();
    assert_eq!(pages, [text_page(1, "first page words")]);
    assert_eq!(end, failed(FailureCode::Internal));
    assert!(
        took >= budget,
        "the page was not given its budget: {took:?}"
    );
    assert!(
        took < Duration::from_secs(10),
        "the page was waited for: {took:?}"
    );
    assert_eq!(
        abandoned.alive(),
        1,
        "the stuck reader is still running, and counted"
    );
    drop(release);
}

/// When nothing finished, nothing true about the file was learnt: a local
/// failure, which another device or a later attempt may not share.
#[test]
fn reading_diagnostics_a_file_whose_every_page_is_lost_fails_locally() {
    let (_one, first) = stuck();
    let (_two, second) = stuck();
    let engine = scripted(vec![first, second], Duration::from_millis(50), lenient());
    let (pages, end) = read(&engine, &request("objects/b3-s", "application/pdf"));
    assert!(pages.is_empty());
    assert_eq!(end, failed(FailureCode::Internal));
}

#[test]
fn reading_diagnostics_a_panicked_page_is_not_marked_complete() {
    let engine = scripted(
        vec![Script::Panics, Script::Words("survivor words")],
        Duration::from_secs(30),
        lenient(),
    );
    let (pages, end) = read(&engine, &request("objects/b3-s", "application/pdf"));
    assert!(pages.is_empty());
    assert_eq!(end, failed(FailureCode::Internal));
}

/// A cancel ends the job between pages, without waiting for the rest.
#[test]
fn a_cancel_ends_the_job_cancelled() {
    let (_release, page_two) = stuck();
    let engine = scripted(
        vec![Script::Words("first page words"), page_two],
        Duration::from_secs(60),
        lenient(),
    );
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut pages = 0;
    let end = run(
        &engine,
        &request("objects/b3-s", "application/pdf"),
        &cancel,
        &mut |_| {
            pages += 1;
            cancel.store(true, Ordering::Release);
            true
        },
    );
    assert_eq!(pages, 1);
    assert_eq!(end, failed(FailureCode::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// Nobody listening is the same as a cancel.
#[test]
fn a_listener_that_goes_away_ends_the_job() {
    let engine = engine(Memory::default().with("objects/b3-pdf", fixtures::two_pages()));
    let cancel = AtomicBool::new(false);
    let end = run(
        &engine,
        &request("objects/b3-pdf", "application/pdf"),
        &cancel,
        &mut |_| false,
    );
    assert_eq!(end, failed(FailureCode::Cancelled));
}

// ------------------------------------------------------------- recognition
//
// Every test below reads through a stand-in reader (`fixtures::FakeReader`):
// what is tested is everything around the model — what is sent to it, in
// what shape, how often, and what the job makes of its answer.

type Recognized = (u32, String, Source, Option<u8>);

/// Run a job; answer every page with its confidence, and the end.
fn read_all(engine: &Engine, job: &ReadJob) -> (Vec<Recognized>, Event) {
    let cancel = AtomicBool::new(false);
    let mut pages = Vec::new();
    let end = run(engine, job, &cancel, &mut |event| {
        match event {
            Event::Page {
                page,
                text,
                source,
                confidence,
                ..
            } => pages.push((page, text.into_string(), source, confidence)),
            other => panic!("only pages are emitted: {other:?}"),
        }
        true
    });
    (pages, end)
}

fn recognize(path: &str, mime: &str) -> ReadJob {
    ReadJob {
        want: Want::Recognize,
        ..request(path, mime)
    }
}

/// A scripted document read with a fake reader that names the page it was
/// handed (a scripted page renders as wide as its number).
fn scripted_reading(pages: Vec<Script>) -> (Engine, fixtures::Seen) {
    let (recognition, seen) =
        fixtures::fake_recognition(|width, _| format!("words read on page {width}"));
    let engine =
        fixtures::scripted_engine(pages, "objects/b3-s", Duration::from_secs(30), lenient())
            .with_recognition(recognition);
    (engine, seen)
}

fn recognized(number: u32) -> Recognized {
    (
        number,
        format!("words read on page {number}"),
        Source::Recognized,
        Some(77),
    )
}

fn layered(number: u32, text: &str) -> Recognized {
    (number, text.to_string(), Source::TextLayer, None)
}

/// **The fall-through**: a page with no usable text layer is rendered and
/// read; a page with one is not sent to the reader.
#[test]
fn a_scanned_page_is_rendered_and_read_and_a_text_page_is_not() {
    let (engine, seen) = scripted_reading(vec![
        Script::Words("typed words on the first page"),
        Script::Scan,
        Script::Words("typed words on the third page"),
    ]);
    let (pages, end) = read_all(&engine, &recognize("objects/b3-s", "application/pdf"));
    assert_eq!(
        pages,
        [
            layered(1, "typed words on the first page"),
            recognized(2),
            layered(3, "typed words on the third page"),
        ]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 3));
    assert_eq!(seen.sizes(), [(2, 1)], "only page 2 reached the reader");
    assert_eq!(engine.recognition.loads(), 1);
}

#[test]
fn reading_diagnostics_force_ocr_ignores_good_embedded_text_and_keeps_caps() {
    let (engine, seen) = scripted_reading(vec![
        Script::Words("plausible but wrong invisible text layer"),
        Script::Words("another plausible but wrong text layer"),
    ]);
    let mut job = recognize("objects/b3-s", "application/pdf");
    job.want = Want::ForceRecognize;
    job.max_read_pages = Some(1);
    let (pages, end) = read_all(&engine, &job);
    assert_eq!(pages, [recognized(1)]);
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 1));
    assert_eq!(seen.sizes(), [(1, 1)]);
}

/// **A page with a real layer and pictures over most of it** is read as well,
/// and keeps both sets of words, once each: `mixed`.
#[test]
fn a_mixed_page_keeps_its_layer_and_the_pictures_words() {
    let (recognition, seen) = fixtures::fake_recognition(|_, _| {
        "Annual figures in a screenshot\nannual figures IN A SCREENSHOT\nRevenue grew by twelve percent".into()
    });
    let engine = fixtures::scripted_engine(
        vec![Script::Mixed("Annual figures in a screenshot")],
        "objects/b3-s",
        Duration::from_secs(30),
        lenient(),
    )
    .with_recognition(recognition);
    let (pages, end) = read_all(&engine, &recognize("objects/b3-s", "application/pdf"));
    assert_eq!(
        pages,
        [(
            1,
            "Annual figures in a screenshot\nRevenue grew by twelve percent".to_string(),
            Source::Mixed,
            Some(77)
        )]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
    assert_eq!(seen.sizes().len(), 1);
}

/// **`Want::Text` is exactly what it was**: with a reader available, a text
/// job sends nothing to it and loads no model.
#[test]
fn a_text_job_never_loads_the_model_even_when_it_could() {
    let (engine, seen) = scripted_reading(vec![Script::Words("typed words here"), Script::Scan]);
    let (pages, end) = read_all(&engine, &request("objects/b3-s", "application/pdf"));
    assert_eq!(pages, [layered(1, "typed words here"), layered(2, "")]);
    assert_eq!(end, done(ReadingStatus::Partial, None, 2));
    assert_eq!(engine.recognition.loads(), 0, "no model was loaded");
    assert!(seen.sizes().is_empty());
}

/// **The reading cap**: at most `max_read_pages` pages are recognized in one
/// job; the job stops before the next one, so a resume starts there. Text
/// pages between them do not count.
#[test]
fn the_reading_cap_stops_before_the_page_past_it_and_text_pages_do_not_count() {
    let pages = || {
        vec![
            Script::Scan,
            Script::Words("typed words on page two"),
            Script::Scan,
            Script::Words("typed words on page four"),
            Script::Scan,
        ]
    };
    let (engine, seen) = scripted_reading(pages());
    let mut job = recognize("objects/b3-s", "application/pdf");
    job.max_read_pages = Some(2);
    let (got, end) = read_all(&engine, &job);
    assert_eq!(
        got,
        [
            recognized(1),
            layered(2, "typed words on page two"),
            recognized(3),
            layered(4, "typed words on page four"),
        ]
    );
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 4));
    assert_eq!(seen.sizes().len(), 2);

    // The resume reads on from page 5, with a cap of its own.
    job.from_page = 5;
    let (got, end) = read_all(&engine, &job);
    assert_eq!(got, [recognized(5)]);
    assert_eq!(end, done(ReadingStatus::Read, None, 5));

    // *Read all pages*: no cap.
    let (engine, _) = scripted_reading(pages());
    let mut job = recognize("objects/b3-s", "application/pdf");
    job.max_read_pages = None;
    let (got, end) = read_all(&engine, &job);
    assert_eq!(got.len(), 5);
    assert_eq!(end, done(ReadingStatus::Read, None, 5));
}

/// A cap reached on the very first page stops before it: nothing read, and
/// `pages_done` is where the job would have started.
#[test]
fn a_cap_of_nothing_reads_nothing() {
    let (engine, seen) = scripted_reading(vec![Script::Scan, Script::Scan]);
    let mut job = recognize("objects/b3-s", "application/pdf");
    job.max_read_pages = Some(0);
    let (got, end) = read_all(&engine, &job);
    assert!(got.is_empty());
    assert_eq!(end, done(ReadingStatus::Partial, Some(Reason::PageCap), 0));
    assert!(seen.sizes().is_empty());
    assert_eq!(koru_read::READ_PAGE_CAP, 200, "the product cap");
}

/// **A page whose images declare too much is never rendered**: the reader is
/// not called and the job carries on with the rest.
#[test]
fn reading_diagnostics_a_page_drawing_a_huge_image_is_not_rendered() {
    let (engine, seen) = scripted_reading(vec![
        Script::Words("typed words on the first page"),
        Script::HugeScan,
        Script::Scan,
    ]);
    let (got, end) = read_all(&engine, &recognize("objects/b3-s", "application/pdf"));
    assert_eq!(got, [layered(1, "typed words on the first page")]);
    assert_eq!(end, failed(FailureCode::Internal));
    assert!(
        seen.sizes().is_empty(),
        "the oversized page never reaches the reader"
    );
}

/// The same, from a real PDF through `hayro`: the declared size comes from
/// the text pass, which decodes nothing.
#[test]
fn reading_diagnostics_a_real_pdfs_decompression_bomb_is_refused_by_its_declared_size() {
    let (recognition, seen) = fixtures::fake_recognition(|_, _| "never".into());
    let engine = engine(Memory::default().with("objects/b3-bomb", fixtures::bomb()))
        .with_recognition(recognition);
    let (got, end) = read_all(&engine, &recognize("objects/b3-bomb", "application/pdf"));
    assert_eq!(got[0].1, "typed words on the first page");
    assert_eq!(got.len(), 1);
    assert_eq!(end, failed(FailureCode::Internal));
    assert!(seen.sizes().is_empty(), "nothing was rendered");
}

/// **A real scan, really rendered**: `hayro` draws the page at 144 dpi and the
/// reader is handed a US-letter page of 1,224 x 1,584 pixels.
#[test]
fn a_real_scanned_page_is_rendered_at_twice_its_size_for_the_reader() {
    let (recognition, seen) =
        fixtures::fake_recognition(|width, height| format!("a page of {width} by {height}"));
    let engine = engine(Memory::default().with("objects/b3-pdf", fixtures::text_then_scan()))
        .with_recognition(recognition);
    let (got, end) = read_all(&engine, &recognize("objects/b3-pdf", "application/pdf"));
    assert_eq!(
        got,
        [
            layered(1, "typed words on the first page"),
            (
                2,
                "a page of 1224 by 1584".to_string(),
                Source::Recognized,
                Some(77)
            ),
        ]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 2));
    assert_eq!(seen.sizes(), [(1224, 1584)]);
}

/// A page recognized to no words is empty, and a file of such pages is
/// `empty`, not `read`.
#[test]
fn a_scan_with_no_words_is_empty() {
    let (recognition, _) = fixtures::fake_recognition(|_, _| String::new());
    let engine = fixtures::scripted_engine(
        vec![Script::Scan],
        "objects/b3-s",
        Duration::from_secs(30),
        lenient(),
    )
    .with_recognition(recognition);
    let (got, end) = read_all(&engine, &recognize("objects/b3-s", "application/pdf"));
    assert_eq!(got, [(1, String::new(), Source::Recognized, Some(77))]);
    assert_eq!(end, done(ReadingStatus::Empty, None, 1));
}

/// A model that will not load ends the job as the contract says a reader-less
/// worker does, and recognition is unavailable from then on.
#[test]
fn a_model_that_will_not_load_ends_the_job_needs_reader() {
    let recognition = crate::recognition::Recognition::with_loader(|| {
        Err(koru_read::ReadError {
            code: FailureCode::Internal,
            message: "no".into(),
        })
    });
    let engine = fixtures::scripted_engine(
        vec![Script::Words("typed words on the first page"), Script::Scan],
        "objects/b3-s",
        Duration::from_secs(30),
        lenient(),
    )
    .with_recognition(recognition);
    let (got, end) = read_all(&engine, &recognize("objects/b3-s", "application/pdf"));
    assert_eq!(got, [layered(1, "typed words on the first page")]);
    assert_eq!(end, failed(FailureCode::NeedsReader));
    assert!(!engine.recognition.available());
}

// ----------------------------------------------------------------- pictures

fn picture_engine(name: &str, bytes: Vec<u8>) -> (Engine, fixtures::Seen) {
    let (recognition, seen) =
        fixtures::fake_recognition(|width, height| format!("a picture of {width} by {height}"));
    (
        engine(Memory::default().with(name, bytes)).with_recognition(recognition),
        seen,
    )
}

/// **A photograph is read upright**: the reader is handed it after its EXIF
/// orientation, and the answer is page 1 with its confidence.
#[test]
fn a_picture_is_read_upright_with_its_confidence() {
    let photo = fixtures::pictures::png_with_exif(&fixtures::pictures::grid(30, 20), Some(6));
    let (engine, seen) = picture_engine("objects/b3-photo", photo);
    assert_eq!(engine.recognition.loads(), 0);
    let (got, end) = read_all(&engine, &recognize("objects/b3-photo", "image/png"));
    assert_eq!(
        got,
        [(
            1,
            "a picture of 20 by 30".to_string(),
            Source::Recognized,
            Some(77)
        )]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
    assert_eq!(seen.sizes(), [(20, 30)], "turned before it was read");
}

/// The recorded type does not decide: a JPEG-labelled PNG is still read.
#[test]
fn a_picture_is_decoded_by_its_bytes() {
    let (engine, seen) = picture_engine(
        "objects/b3-p",
        fixtures::pictures::png(&fixtures::pictures::grid(4, 4)),
    );
    let (_, end) = read_all(&engine, &recognize("objects/b3-p", "image/jpeg"));
    assert_eq!(end, done(ReadingStatus::Read, None, 1));
    assert_eq!(seen.sizes(), [(4, 4)]);
}

#[test]
fn a_picture_with_no_words_is_empty() {
    let (recognition, _) = fixtures::fake_recognition(|_, _| String::new());
    let engine = engine(Memory::default().with(
        "objects/b3-p",
        fixtures::pictures::png(&fixtures::pictures::grid(4, 4)),
    ))
    .with_recognition(recognition);
    let (got, end) = read_all(&engine, &recognize("objects/b3-p", "image/png"));
    assert_eq!(got, [(1, String::new(), Source::Recognized, Some(77))]);
    assert_eq!(end, done(ReadingStatus::Empty, None, 1));
}

/// Broken bytes in a known format are damaged on every device; a format this
/// worker does not read, or a bomb, is a local refusal; and none of them
/// reaches the reader.
#[test]
fn a_damaged_an_unsupported_and_a_huge_picture_never_reach_the_reader() {
    let mut broken = fixtures::pictures::png(&fixtures::pictures::grid(8, 8));
    broken.truncate(broken.len() / 2);
    let (engine, seen) = picture_engine("objects/b3-p", broken);
    let (got, end) = read_all(&engine, &recognize("objects/b3-p", "image/png"));
    assert!(got.is_empty());
    assert_eq!(end, done(ReadingStatus::Failed, Some(Reason::Damaged), 0));
    assert!(seen.sizes().is_empty());

    let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
    let (engine, _) = picture_engine("objects/b3-h", heic);
    assert_eq!(
        read_all(&engine, &recognize("objects/b3-h", "image/heic")).1,
        failed(FailureCode::Unsupported)
    );

    let (engine, seen) = picture_engine(
        "objects/b3-b",
        fixtures::pictures::png_declaring(20_000, 20_000),
    );
    assert_eq!(
        read_all(&engine, &recognize("objects/b3-b", "image/png")).1,
        failed(FailureCode::Unsupported)
    );
    assert!(seen.sizes().is_empty());
}

/// Asked for text only, a picture needs the reader and nothing is decoded or
/// loaded.
#[test]
fn a_picture_asked_for_text_loads_nothing() {
    let (engine, seen) = picture_engine(
        "objects/b3-p",
        fixtures::pictures::png(&fixtures::pictures::grid(4, 4)),
    );
    let (got, end) = read_all(&engine, &request("objects/b3-p", "image/png"));
    assert!(got.is_empty());
    assert_eq!(end, failed(FailureCode::NeedsReader));
    assert_eq!(engine.recognition.loads(), 0);
    assert!(seen.sizes().is_empty());
}
