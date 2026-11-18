// SPDX-License-Identifier: MPL-2.0

//! **What an import tells the reader, and what its summary offers** (OCR Plan
//! 3.3; EXPERIENCE §3 "The first time" and *Not now*; TECHNICAL §7
//! `offer_declined`, §10).
//!
//! Pictures and scanned pages need the reader, a one-time download that Koru
//! asks for once per computer, where the person is when it first matters. One
//! of those places is **the end of an import**: an import through an importer
//! (an Evernote archive, a Notion export, a OneNote notebook…) reports to the
//! Store how many pictures and scanned PDFs it brought, once, from its own
//! worker thread. A non-zero count is what clears a *Not now* — "Koru offers
//! once more after the next import that brings pictures or scanned PDFs" — and
//! the Store answers with the reader's state, which the import's summary uses
//! to decide whether it gains its line.
//!
//! **Adding or pasting files never reports.** Dropping, pasting, `/attach`,
//! a share from the phone and a file an assistant adds all run through the
//! `files` importer, and "adding or pasting single files never brings it
//! back" (EXPERIENCE §3). [`reports_to_reader`] is that rule, in one place.
//!
//! ## What is counted
//!
//! The design counts "pictures and scanned PDFs", and whether a PDF is a scan
//! cannot be known at import time without reading it — which is the reader's
//! job, and the import must not do it. So **every PDF the import stored
//! counts**, beside every picture of a kind the reader reads (PNG, JPEG, GIF,
//! WebP, BMP, TIFF — EXPERIENCE §3 "What counts in rung one"; HEIC is not
//! read). The count is taken where the bytes go — the import's blob sink —
//! so it is the same for every importer and needs no importer to know about
//! the reader. Each distinct blob counts once, by its content address. Nothing
//! else leaves: no name, no path, no title; the Store hears a number.
//!
//! ## Nothing here runs on a thread that answers a person
//!
//! [`report_import`] is called from the import's worker; the answers to what
//! a person presses on the line are carried out on a thread of their own in
//! [`crate::app`]. The window is handed values.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use koru_store_wire::reader::{CatalogStage, ImportFinished, InstallReader, ReaderState};

/// **What an import's summary carries about the reader**: how many pictures
/// and scanned PDFs the import brought, and the reader's state as the Store
/// answered that report — the state that already says whether a *Not now* was
/// cleared. The window words the line from these alone
/// (`koru_search_ui::import_summary`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReading {
    pub pictures_and_scans: u64,
    pub state: ReaderState,
}

/// **What a person pressed on the line in an import's summary**, or on the
/// download it started. Carried out by the shell, off the event loop; every
/// answer comes back to the window as the reader's state
/// (`View::reader_answered`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderAnswer {
    /// **Turn on** (or **Try again**): the terms the person was shown, sent
    /// back verbatim, from the offer.
    TurnOn(InstallReader),
    /// **Not now**: the line goes quiet until the next import that brings
    /// pictures or scanned PDFs.
    NotNow,
    /// **Stop** the download Turn on started.
    Stop,
}

/// Whether an import of this format reports to the reader. Every importer
/// does; the `files` importer — adding, pasting, attaching or sharing single
/// files — never does.
pub(crate) fn reports_to_reader(format: &str) -> bool {
    format != "files"
}

/// Whether a blob of this media type is a picture or a PDF the import brought
/// for the reader to read. See the module's *What is counted*.
pub(crate) fn counts_for_reader(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-ms-bmp"
            | "image/tiff"
            | "image/tif"
    )
}

/// **The import's blob sink, counting as it stores.** Every byte an importer
/// keeps goes through its sink, so this is the one place the count is true
/// for every importer. It stores exactly what it is given and counts each
/// distinct picture or PDF once, by the address the Store gave it.
pub(crate) struct CountingBlobSink<'a> {
    inner: &'a dyn koru_import::BlobSink,
    seen: Mutex<HashSet<String>>,
}

impl<'a> CountingBlobSink<'a> {
    pub(crate) fn new(inner: &'a dyn koru_import::BlobSink) -> Self {
        Self {
            inner,
            seen: Mutex::new(HashSet::new()),
        }
    }

    /// How many pictures and PDFs have been stored through this sink.
    pub(crate) fn count(&self) -> u64 {
        self.seen.lock().map_or(0, |seen| seen.len() as u64)
    }

    fn noted(
        &self,
        stored: Result<koru_import::StoredBlob, koru_import::ImportError>,
        asked: &str,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        if let Ok(blob) = &stored
            // The Store's media type, falling back to the importer's: the
            // Store may sniff a better one than the archive declared.
            && (counts_for_reader(&blob.mime) || (blob.mime.is_empty() && counts_for_reader(asked)))
            && let Ok(mut seen) = self.seen.lock()
        {
            seen.insert(blob.hash.clone());
        }
        stored
    }
}

impl koru_import::BlobSink for CountingBlobSink<'_> {
    fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        self.noted(self.inner.put(bytes, mime, filename), mime)
    }

    fn put_from(
        &self,
        factory: &(dyn Fn() -> Box<dyn std::io::Read + Send> + Send + Sync),
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        self.noted(self.inner.put_from(factory, mime, filename), mime)
    }
}

/// The two questions an import asks the Store about the reader. A trait so
/// the rule — once, with the count, never for single files — is tested
/// without a Store.
pub(crate) trait ReaderReport {
    /// `POST /v1/readings/reader/imported`.
    fn import_finished(&self, report: &ImportFinished) -> Result<ReaderState, String>;
    /// `GET /v1/readings/reader`.
    fn reader(&self) -> Result<ReaderState, String>;
}

impl ReaderReport for koru_api_client::Client {
    fn import_finished(&self, report: &ImportFinished) -> Result<ReaderState, String> {
        self.store()
            .reader_import_finished(report)
            .map_err(|error| error.code().as_str().to_owned())
    }

    fn reader(&self) -> Result<ReaderState, String> {
        self.store()
            .reader()
            .map_err(|error| error.code().as_str().to_owned())
    }
}

/// How long, at most, an import that brought pictures waits at its end for
/// the Store to finish reading the reader's catalog, so its summary can say
/// the real size. The first read of the state is what starts that fetch;
/// [`prime`] asks it as soon as the import has stored its last blob, so this
/// is usually not waited at all.
const CATALOG_WAITS: u32 = 4;
const CATALOG_WAIT: Duration = Duration::from_millis(500);

/// Ask the reader's state once, so the Store starts reading its catalog
/// while the import writes its documents. Only when the import brought
/// something to offer for, and only for an importer.
pub(crate) fn prime(format: &str, pictures_and_scans: u64, reporter: &dyn ReaderReport) {
    if reports_to_reader(format) && pictures_and_scans > 0 {
        let _ = reporter.reader();
    }
}

/// **The import's report to the reader**: once, at its end, with its count —
/// for an importer, and never for the `files` importer. Returns what the
/// summary needs, or `None` when nothing was reported or the Store did not
/// answer (the summary then simply has no line; the line in search is still
/// there for the next search).
///
/// When the import brought pictures and the Store has not finished reading
/// the reader's catalog, waits a bounded moment for the size, on the
/// import's own worker thread.
pub(crate) fn report_import(
    format: &str,
    pictures_and_scans: u64,
    reporter: &dyn ReaderReport,
    wait: &dyn Fn(Duration),
) -> Option<ImportReading> {
    if !reports_to_reader(format) {
        return None;
    }
    let mut state = reporter
        .import_finished(&ImportFinished { pictures_and_scans })
        .ok()?;
    let mut waits = 0;
    while pictures_and_scans > 0 && catalog_pending(&state) && waits < CATALOG_WAITS {
        wait(CATALOG_WAIT);
        waits += 1;
        match reporter.reader() {
            Ok(next) => state = next,
            Err(_) => break,
        }
    }
    Some(ImportReading {
        pictures_and_scans,
        state,
    })
}

/// The reader could be offered and its size is not known yet.
fn catalog_pending(state: &ReaderState) -> bool {
    state.installed.is_none()
        && state.unavailable.is_none()
        && state.offer.is_none()
        && matches!(
            state.catalog.state,
            CatalogStage::Unread | CatalogStage::Reading
        )
}

/// How often a download Turn on started is asked about. The same as the line
/// in search.
pub(crate) const INSTALL_POLL: Duration = Duration::from_millis(500);

/// Whether a state says a download is still going, so it is asked about
/// again.
pub(crate) fn installing(state: &ReaderState) -> bool {
    state
        .install
        .as_ref()
        .is_some_and(|install| install.stage.is_running())
}

#[cfg(test)]
mod tests;
