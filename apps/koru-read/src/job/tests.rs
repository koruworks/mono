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
    let mut recognize = request("objects/b3-png", "image/png");
    recognize.want = Want::Recognize;
    assert_eq!(
        read(&engine, &recognize).1,
        failed(FailureCode::Unsupported)
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
/// carries on with the rest: the lost page is progress with no words, and
/// the reading is made from the pages that finished.
#[test]
fn a_page_that_never_finishes_is_abandoned_at_its_budget() {
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
    assert_eq!(
        pages,
        [
            text_page(1, "first page words"),
            text_page(2, ""),
            text_page(3, "third page words")
        ]
    );
    assert_eq!(end, done(ReadingStatus::Read, None, 3));
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
fn a_file_whose_every_page_is_lost_fails_locally() {
    let (_one, first) = stuck();
    let (_two, second) = stuck();
    let engine = scripted(vec![first, second], Duration::from_millis(50), lenient());
    let (pages, end) = read(&engine, &request("objects/b3-s", "application/pdf"));
    assert_eq!(pages, [text_page(1, ""), text_page(2, "")]);
    assert_eq!(end, failed(FailureCode::Internal));
}

#[test]
fn a_page_that_panics_is_skipped_and_the_rest_are_read() {
    let engine = scripted(
        vec![Script::Panics, Script::Words("survivor words")],
        Duration::from_secs(30),
        lenient(),
    );
    let (pages, end) = read(&engine, &request("objects/b3-s", "application/pdf"));
    assert_eq!(pages, [text_page(1, ""), text_page(2, "survivor words")]);
    assert_eq!(end, done(ReadingStatus::Read, None, 2));
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
