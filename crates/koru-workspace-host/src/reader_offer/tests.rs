// SPDX-License-Identifier: MPL-2.0

//! The import's report to the reader — once, with its count, never for single
//! files — and the count itself, taken where the bytes go. No Store, no disk.

use std::cell::Cell;
use std::sync::Mutex;

use koru_import::BlobSink;
use koru_store_wire::reader::{CatalogState, ReaderTerms};

use super::*;

/// A sink that stores nothing and answers with an address derived from the
/// bytes, as the Store's is.
struct FakeSink;

impl BlobSink for FakeSink {
    fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        _filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        if bytes.is_empty() {
            return Err(koru_import::ImportError::new("empty"));
        }
        Ok(koru_import::StoredBlob {
            hash: format!(
                "b3:{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
            mime: mime.to_owned(),
        })
    }
}

/// What the Store was asked, in order.
#[derive(Default)]
struct FakeStore {
    calls: Mutex<Vec<String>>,
    answers: Mutex<Vec<ReaderState>>,
}

impl FakeStore {
    fn answering(states: Vec<ReaderState>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            answers: Mutex::new(states),
        }
    }

    fn next(&self) -> ReaderState {
        let mut answers = self.answers.lock().unwrap();
        if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers.first().cloned().unwrap_or_default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl ReaderReport for FakeStore {
    fn import_finished(&self, report: &ImportFinished) -> Result<ReaderState, String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("imported {}", report.pictures_and_scans));
        Ok(self.next())
    }

    fn reader(&self) -> Result<ReaderState, String> {
        self.calls.lock().unwrap().push("reader".into());
        Ok(self.next())
    }
}

fn offered() -> ReaderState {
    ReaderState {
        offer: Some(ReaderTerms {
            model: "pp-ocrv6-small".into(),
            version: "1".into(),
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
            download_bytes: 24_536_678,
            languages: vec!["und-Latn".into()],
        }),
        catalog: CatalogState {
            state: CatalogStage::Ready,
            code: None,
        },
        ..ReaderState::default()
    }
}

fn reading_catalog() -> ReaderState {
    ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Reading,
            code: None,
        },
        ..ReaderState::default()
    }
}

fn no_wait(_: Duration) {}

/// **Every importer reports; adding or pasting files never does.**
#[test]
fn only_an_import_through_an_importer_reports() {
    for format in [
        "evernote",
        "notion",
        "onenote",
        "markdown",
        "textbundle",
        "google-docs",
    ] {
        assert!(reports_to_reader(format), "{format}");
    }
    assert!(!reports_to_reader("files"));
}

/// **An import calls the Store exactly once, with its count**, and its
/// summary carries what the Store answered.
#[test]
fn an_import_reports_once_with_its_count() {
    let store = FakeStore::answering(vec![offered()]);
    let reading = report_import("evernote", 412, &store, &no_wait).unwrap();
    assert_eq!(store.calls(), ["imported 412"]);
    assert_eq!(
        reading,
        ImportReading {
            pictures_and_scans: 412,
            state: offered(),
        }
    );
    // An import that brought none still reports — with zero, which clears
    // nothing — and waits for nothing.
    let quiet = FakeStore::answering(vec![reading_catalog()]);
    let reading = report_import("notion", 0, &quiet, &no_wait).unwrap();
    assert_eq!(quiet.calls(), ["imported 0"]);
    assert_eq!(reading.pictures_and_scans, 0);
}

/// **A single add or paste never calls it** — not to report, not to ask.
#[test]
fn adding_or_pasting_files_never_calls_the_store() {
    let store = FakeStore::answering(vec![offered()]);
    assert_eq!(report_import("files", 3, &store, &no_wait), None);
    prime("files", 3, &store);
    assert!(store.calls().is_empty(), "{:?}", store.calls());
}

/// The catalog still being read is waited for, a bounded number of times,
/// so the summary can say the real size; and only when there is something to
/// offer for.
#[test]
fn a_catalog_still_being_read_is_waited_for_a_moment_and_no_longer() {
    let waits = Cell::new(0);
    let wait = |_: Duration| waits.set(waits.get() + 1);
    let store = FakeStore::answering(vec![reading_catalog(), reading_catalog(), offered()]);
    let reading = report_import("evernote", 9, &store, &wait).unwrap();
    assert_eq!(store.calls(), ["imported 9", "reader", "reader"]);
    assert_eq!(waits.get(), 2);
    assert_eq!(reading.state, offered());

    waits.set(0);
    let never = FakeStore::answering(vec![reading_catalog()]);
    let reading = report_import("evernote", 9, &never, &wait).unwrap();
    assert_eq!(waits.get(), CATALOG_WAITS, "bounded");
    assert_eq!(
        reading.state,
        reading_catalog(),
        "no size: the summary has no line"
    );
}

/// Priming asks the state once, for an importer that brought something.
#[test]
fn priming_asks_once_and_only_when_it_matters() {
    let store = FakeStore::answering(vec![reading_catalog()]);
    prime("evernote", 0, &store);
    assert!(store.calls().is_empty());
    prime("evernote", 2, &store);
    assert_eq!(store.calls(), ["reader"]);
}

/// **The count is taken where the bytes go**: each distinct picture or PDF
/// once, whatever the importer; other kinds of file never; a blob that was
/// not stored never. What is stored is exactly what the importer asked.
#[test]
fn the_sink_counts_each_distinct_picture_and_pdf_once() {
    let counted = CountingBlobSink::new(&FakeSink);
    let png = counted.put(vec![1, 2], "image/png", Some("a.png")).unwrap();
    assert_eq!(png.mime, "image/png");
    counted
        .put(vec![1, 2], "image/png", Some("copy.png"))
        .unwrap();
    counted.put(vec![3], "application/pdf", None).unwrap();
    counted
        .put(vec![4], "image/jpeg; charset=binary", None)
        .unwrap();
    counted.put(vec![5], "text/markdown", None).unwrap();
    counted.put(vec![6], "image/heic", None).unwrap();
    counted.put(vec![7], "image/svg+xml", None).unwrap();
    assert!(counted.put(Vec::new(), "image/png", None).is_err());
    let factory = || -> Box<dyn std::io::Read + Send> { Box::new(std::io::Cursor::new(vec![8u8])) };
    counted.put_from(&factory, "image/TIFF", None).unwrap();
    assert_eq!(
        counted.count(),
        4,
        "the png once, the pdf, the jpeg, the tiff"
    );
}

#[test]
fn the_kinds_that_count_are_the_ones_the_reader_reads() {
    for mime in [
        "application/pdf",
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/tiff",
    ] {
        assert!(counts_for_reader(mime), "{mime}");
    }
    for mime in ["image/heic", "text/plain", "application/zip", ""] {
        assert!(!counts_for_reader(mime), "{mime}");
    }
}
