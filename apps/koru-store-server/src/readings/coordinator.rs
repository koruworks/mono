// SPDX-License-Identifier: MPL-2.0

//! **The coordinator: what to read, a worker to read it, and the words written
//! into each entry's reading document** (TECHNICAL D11, §9).
//!
//! ## The queue is a query
//!
//! Recomputed, never persisted (TECHNICAL §9 "What the coordinator queues").
//! The unit is a pair — a live entry and a blob it references — and a pair is
//! queued when:
//!
//! - the entry is a page or a file, not in the Trash, and the node carrying
//!   the blob is live in it (`Projections::readable_references`);
//! - the blob is readable *by text* in this plan — a PDF or a plain-text
//!   file. A picture needs the reader, which this plan does not have: it is
//!   counted `needs_reader` and not sent;
//! - the entry's reading document has no settled entry for the blob at this
//!   build's generation (the mirror's `settled_in`, checked again against the
//!   document itself just before the job);
//! - nothing in the local attempts table holds it back;
//! - the Workspace may read the bytes and they are on this device. A blob
//!   whose bytes are absent is counted `not_here`.
//!
//! Smallest first, by the blob row's size, with a freshly added file ahead of
//! everything. A restart computes the same queue from the same state.
//!
//! ## Told, not polling
//!
//! The two triggers (ROADMAP §3) arrive through the [`Inbox`]: a projected
//! document that references a blob, and bytes landing. Each is resolved with
//! one targeted query into **fresh** pairs at the front. A listing change marks
//! the whole queue owed; it is recomputed when the queue empties, or at most
//! every [`RECOMPUTE_EVERY_MILLIS`] while it runs, so a large import is not a
//! full recompute per file — the fresh lane finds each new file at once.
//!
//! ## One job at a time, and the Store lock never across one
//!
//! For each pair: copy the words if this store root already holds them for
//! these bytes; otherwise ensure the entry's reading document
//! (`Store::ensure_reading`, or `ensure_readings` for a batch of the backlog),
//! fork it, write through [`koru_read::ReadingDoc`] outside the lock, and apply
//! the delta through the Store's ordinary update path with the Store as its
//! origin, flushed like any commit, broadcast and announced to peers. The
//! Store lock is held for a fork, a registration or an apply — never across a
//! worker round-trip, a file read or a database write of the mirror's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use koru_core::explore::KIND_READING;
use koru_projection::{Projections, ReadableReference, ReadableScope};
use koru_read::{
    Event, FailureCode, GENERATION, JobId, Outcome, PageText, READER_TEXT, ReaderId, Reading,
    ReadingDoc, ReadingStatus, Request, TEXT_PAGE_CAP, Want, Write,
};
use koru_store_api::state::AppState;
use koru_store_core::ids::{DocumentId, WorkspaceId, reading_document_id};
use koru_store_core::model::Timestamp;
use koru_store_core::ports::UpdateOrigin;

use super::feed::{CoordinatorWork, Inbox};
use super::launcher::{Availability, Interrupted, Launcher, Unavailable};
use super::mirror::{Attempt, AttemptCode, Mirror};

/// While a queue is running, a listing change recomputes it at most this
/// often. New files reach the fresh lane at once regardless; this only bounds
/// how often the backlog's order and counts are redone during an import.
pub const RECOMPUTE_EVERY_MILLIS: i64 = 2_000;

/// A backlog registers at most this many reading documents in one manifest
/// commit (TECHNICAL §9 "a few hundred at a time").
pub const REGISTER_BATCH: usize = 200;

/// Pages written into a reading document per Store commit while a long file
/// is read. Plan 0.1 measured a text layer at 182–440 pages a second, so the
/// commits are what costs, and a typical file (two pages) is one commit.
pub const FLUSH_PAGES: usize = 64;

/// Local failures that are retried at all, before the file is left until the
/// next generation.
const MAX_RETRIES: u32 = 3;

/// The first backoff after a retryable local failure; each further failure
/// quadruples it, up to an hour.
const BACKOFF_MILLIS: i64 = 60_000;
const BACKOFF_CEILING_MILLIS: i64 = 60 * 60_000;

/// A backlog this long is news when it finishes (TECHNICAL §14: "a backlog
/// finished (one line, with counts)").
const BACKLOG_NEWS: u64 = 25;

/// What kind of reading a blob needs, by its media type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Readable {
    /// Words already in the file: a PDF's text layer, a plain-text file.
    Text,
    /// Pixels: needs the reader, which this plan does not run.
    Picture,
}

/// The coarse filter the queue applies. The worker trusts the bytes over the
/// media type; this only decides what is worth sending.
pub fn readable(mime: &str) -> Option<Readable> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "application/pdf" | "text/plain" | "text/markdown" | "text/x-markdown" | "text/csv"
        | "text/html" => Some(Readable::Text),
        other if other.starts_with("image/") => Some(Readable::Picture),
        _ => None,
    }
}

/// Where the bytes are, and whether they are here.
///
/// **Bytes are not permission**: the coordinator asks the Store for the
/// Workspace's `blobs` row first, and only then whether the object is on the
/// disk. This seam answers the second half, outside the Store lock — a file's
/// existence is I/O the Store lock must not wait on.
pub trait Objects: Send {
    /// Whether the verified object for `hash` is on this device.
    fn here(&self, hash: &str) -> bool;
    /// The object the worker opens — content-addressed, never a file's name.
    fn path(&self, hash: &str) -> Option<PathBuf>;
}

/// A device's objects: `<root>/blobs/objects/<algorithm>-<digest>`.
pub struct ObjectDir(pub PathBuf);

impl Objects for ObjectDir {
    fn here(&self, hash: &str) -> bool {
        self.path(hash).is_some_and(|path| path.is_file())
    }

    fn path(&self, hash: &str) -> Option<PathBuf> {
        let hash = koru_store_core::model::BlobHash::parse(hash).ok()?;
        Some(self.0.join(hash.file_name()))
    }
}

/// One entry and a blob it references, ready to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub workspace: WorkspaceId,
    pub parent: DocumentId,
    pub hash: String,
    pub mime: String,
    pub size: u64,
    pub fresh: bool,
}

impl Pair {
    fn key(&self) -> (u64, WorkspaceId, DocumentId, String) {
        (self.size, self.workspace, self.parent, self.hash.clone())
    }
}

/// What is queued, and why the rest is not — the debug route's first answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct QueueCounts {
    pub fresh: u64,
    pub backlog: u64,
    pub needs_reader: u64,
    pub not_here: u64,
    pub trashed: u64,
    pub backoff: u64,
    /// Held back by a local failure until the next generation.
    pub failed: u64,
    /// Queued, and waiting because no reader can be asked.
    pub waiting_for_reader: u64,
}

/// What the coordinator has done since it started. Counts only.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CoordinatorStats {
    pub read: u64,
    pub empty: u64,
    pub partial: u64,
    pub failed: u64,
    pub copied: u64,
    pub skipped: u64,
    pub pages: u64,
    pub registered: u64,
    pub commits: u64,
    pub failed_by_code: std::collections::BTreeMap<String, u64>,
    pub recomputes: u64,
    pub last_recompute_millis: u64,
    pub last_error: Option<String>,
}

/// The job running now, for the debug route.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CurrentJob {
    pub job: u64,
    pub stage: &'static str,
    pub from_page: u32,
    pub page: u32,
}

/// Shared with the board, so a long job's page is visible while it runs.
pub type Current = Arc<Mutex<Option<CurrentJob>>>;

/// Why a job stopped short of writing.
enum Processed {
    Done,
    NoReader(Unavailable),
}

pub struct Coordinator {
    state: Weak<AppState>,
    projections: Projections,
    mirror: Mirror,
    launcher: Box<dyn Launcher>,
    objects: Box<dyn Objects>,
    inbox: Arc<Inbox>,
    plan: VecDeque<Pair>,
    waiting: Vec<Pair>,
    pub counts: QueueCounts,
    computed: bool,
    owed: bool,
    last_recompute: i64,
    /// Pairs whose commit failed (a full disk), not before this instant. In
    /// memory: a restart retries them, which is what a person would want.
    retry_after: HashMap<(DocumentId, String), i64>,
    /// Readings this coordinator finished since its last recompute, by hash:
    /// the copy rule's answer for bytes the mirror has not applied yet — a
    /// backlog holding the same picture twice in a row would otherwise read it
    /// twice. Cleared on every recompute, by when the mirror holds them.
    written: HashMap<String, (WorkspaceId, DocumentId)>,
    next_job: u64,
    backlog_seen: u64,
    pub stats: CoordinatorStats,
    pub current: Current,
    pub worker_down: Option<Unavailable>,
    debug: bool,
    on_progress: Option<Box<dyn Fn() + Send>>,
}

impl Coordinator {
    pub fn new(
        state: &Arc<AppState>,
        projections: Projections,
        mirror: Mirror,
        launcher: Box<dyn Launcher>,
        objects: Box<dyn Objects>,
        inbox: Arc<Inbox>,
    ) -> Self {
        Self {
            state: Arc::downgrade(state),
            projections,
            mirror,
            launcher,
            objects,
            inbox,
            plan: VecDeque::new(),
            waiting: Vec::new(),
            counts: QueueCounts::default(),
            computed: false,
            owed: true,
            last_recompute: i64::MIN,
            retry_after: HashMap::new(),
            written: HashMap::new(),
            next_job: 1,
            backlog_seen: 0,
            stats: CoordinatorStats::default(),
            current: Arc::new(Mutex::new(None)),
            worker_down: None,
            debug: koru_env::switch(koru_env::name::KORU_READ_DEBUG),
            on_progress: None,
        }
    }

    /// Called while a long job runs, every [`FLUSH_PAGES`] pages, so the
    /// debug route sees the page move.
    pub fn on_progress(&mut self, publish: impl Fn() + Send + 'static) {
        self.on_progress = Some(Box::new(publish));
    }

    pub fn launcher(&mut self) -> &mut dyn Launcher {
        self.launcher.as_mut()
    }

    pub fn launcher_report(&self) -> super::launcher::WorkerReport {
        self.launcher.report()
    }

    /// The queue as it stands, front first.
    #[cfg(test)]
    pub fn queued(&self) -> Vec<Pair> {
        self.plan.iter().cloned().collect()
    }

    /// One unit of work. Returns whether there may be more right now.
    pub fn step(&mut self, now: i64) -> bool {
        let work = self.inbox.take_coordinator();
        self.owed |= work.dirty;
        // The first pass is the full query, so the projector's start-up
        // sweep — which projects every document — does not make every file
        // look freshly added.
        if !self.computed {
            self.recompute(now);
        }
        // Triggers before a recompute they may have caused: a new file is
        // fresh because the plan did not hold it, and the recompute keeps it
        // fresh.
        if !work.documents.is_empty() || !work.hashes.is_empty() {
            self.take_hints(work, now);
        }
        if self.owed
            && (self.plan.is_empty()
                || now.saturating_sub(self.last_recompute) >= RECOMPUTE_EVERY_MILLIS)
        {
            self.recompute(now);
        }
        if !self.waiting.is_empty() && matches!(self.launcher.availability(), Availability::Ready) {
            // A reader is back: what waited goes first, in its old order.
            for pair in self.waiting.drain(..).rev() {
                self.plan.push_front(pair);
            }
            self.counts.waiting_for_reader = 0;
            self.worker_down = None;
        }
        let Some(pair) = self.plan.pop_front() else {
            self.finish_backlog();
            return false;
        };
        if pair.fresh {
            self.counts.fresh = self.counts.fresh.saturating_sub(1);
        } else {
            self.counts.backlog = self.counts.backlog.saturating_sub(1);
            self.backlog_seen += 1;
        }
        match self.process(&pair, now) {
            Processed::Done => {}
            Processed::NoReader(why) => {
                self.worker_down = Some(why);
                self.counts.waiting_for_reader += 1;
                self.waiting.push(pair);
            }
        }
        true
    }

    /// Say once when a backlog of any size drains (TECHNICAL §14).
    fn finish_backlog(&mut self) {
        if self.backlog_seen >= BACKLOG_NEWS {
            tracing::info!(
                target: "koru_readings",
                files = self.backlog_seen,
                read = self.stats.read,
                copied = self.stats.copied,
                partial = self.stats.partial,
                failed = self.stats.failed,
                "a reading backlog finished"
            );
        }
        self.backlog_seen = 0;
    }

    // ---------------------------------------------------------- the query

    /// **The queue, recomputed from the projection, the mirror, the attempts
    /// table and the Store's blob rows.** Nothing of the previous plan is
    /// kept: a restart and this are the same computation.
    pub fn recompute(&mut self, now: i64) {
        let started = std::time::Instant::now();
        self.owed = false;
        self.computed = true;
        self.last_recompute = now;
        let mut counts = QueueCounts::default();
        let mut plan = Vec::new();
        let attempts = self.mirror.attempts(READER_TEXT).unwrap_or_default();
        let workspaces = match self.projections.listings() {
            Ok(listings) => listings.into_iter().map(|(w, _)| w).collect::<Vec<_>>(),
            Err(error) => {
                self.stats.last_error = Some(error.0);
                Vec::new()
            }
        };
        for workspace in workspaces {
            let references = match self
                .projections
                .readable_references(workspace, ReadableScope::Workspace)
            {
                Ok(references) => references,
                Err(error) => {
                    self.stats.last_error = Some(error.0);
                    continue;
                }
            };
            self.classify(
                workspace,
                references,
                false,
                &attempts,
                now,
                &mut plan,
                &mut counts,
            );
        }
        plan.sort_by_key(Pair::key);
        // **Fresh stays fresh.** A file added since the last pass keeps its
        // place ahead of the backlog through the recompute its own listing
        // change caused.
        let fresh: HashSet<(DocumentId, String)> = self
            .plan
            .iter()
            .chain(self.waiting.iter())
            .filter(|p| p.fresh)
            .map(|p| (p.parent, p.hash.clone()))
            .collect();
        for pair in &mut plan {
            pair.fresh = fresh.contains(&(pair.parent, pair.hash.clone()));
        }
        plan.sort_by_key(|p| (!p.fresh, p.key()));
        counts.fresh = plan.iter().filter(|p| p.fresh).count() as u64;
        counts.backlog = plan.len() as u64 - counts.fresh;
        // What is waiting for a reader is in the plan again; it waits again
        // the moment it is reached if none is back.
        self.waiting.clear();
        self.written.clear();
        self.plan = plan.into();
        self.counts = counts;
        self.stats.recomputes += 1;
        self.stats.last_recompute_millis = started.elapsed().as_millis() as u64;
    }

    /// Turn the two triggers into fresh pairs at the front.
    fn take_hints(&mut self, work: CoordinatorWork, now: i64) {
        let attempts = self.mirror.attempts(READER_TEXT).unwrap_or_default();
        let mut by_workspace: HashMap<WorkspaceId, Vec<DocumentId>> = HashMap::new();
        for (workspace, document) in work.documents {
            by_workspace.entry(workspace).or_default().push(document);
        }
        let mut fresh = Vec::new();
        let mut ignored = QueueCounts::default();
        for (workspace, documents) in by_workspace {
            if let Ok(references) = self
                .projections
                .readable_references(workspace, ReadableScope::Documents(&documents))
            {
                self.classify(
                    workspace,
                    references,
                    true,
                    &attempts,
                    now,
                    &mut fresh,
                    &mut ignored,
                );
            }
        }
        for (workspace, hashes) in work.hashes {
            if let Ok(references) = self
                .projections
                .readable_references(workspace, ReadableScope::Carrying(&hashes))
            {
                self.classify(
                    workspace,
                    references,
                    true,
                    &attempts,
                    now,
                    &mut fresh,
                    &mut ignored,
                );
            }
        }
        if fresh.is_empty() {
            return;
        }
        fresh.sort_by_key(Pair::key);
        fresh.dedup_by(|a, b| a.parent == b.parent && a.hash == b.hash);
        // **Only what the queue did not already hold is fresh.** A document
        // re-projected for any other reason — an edit beside the picture, a
        // sweep — must not lift its backlog pair ahead of the files that
        // really were just added.
        let known: HashSet<(DocumentId, String)> = self
            .plan
            .iter()
            .chain(self.waiting.iter())
            .map(|p| (p.parent, p.hash.clone()))
            .collect();
        // Behind the fresh pairs already queued, ahead of the backlog.
        let mut at = self.plan.iter().take_while(|p| p.fresh).count();
        for pair in fresh {
            if known.contains(&(pair.parent, pair.hash.clone())) {
                continue;
            }
            self.counts.fresh += 1;
            self.plan.insert(at.min(self.plan.len()), pair);
            at += 1;
        }
    }

    /// Decide, for each reference, whether it is queued and why not.
    #[allow(clippy::too_many_arguments)]
    fn classify(
        &mut self,
        workspace: WorkspaceId,
        references: Vec<ReadableReference>,
        fresh: bool,
        attempts: &HashMap<String, Attempt>,
        now: i64,
        out: &mut Vec<Pair>,
        counts: &mut QueueCounts,
    ) {
        if references.is_empty() {
            return;
        }
        let settled = self.mirror.settled_in(workspace).unwrap_or_default();
        let mut candidates = Vec::new();
        for reference in references {
            if reference.trashed {
                counts.trashed += 1;
                continue;
            }
            if settled.contains(&(reference.document, reference.blob.clone())) {
                continue;
            }
            if let Some(mime) = reference.mime.as_deref() {
                match readable(mime) {
                    None => continue,
                    Some(Readable::Picture) => {
                        counts.needs_reader += 1;
                        continue;
                    }
                    Some(Readable::Text) => {}
                }
            }
            if let Some(attempt) = attempts.get(&reference.blob) {
                match verdict(attempt, now) {
                    Verdict::Go => {}
                    Verdict::NeedsReader => {
                        counts.needs_reader += 1;
                        continue;
                    }
                    Verdict::Backoff => {
                        counts.backoff += 1;
                        continue;
                    }
                    Verdict::Failed => {
                        counts.failed += 1;
                        continue;
                    }
                }
            }
            if self
                .retry_after
                .get(&(reference.document, reference.blob.clone()))
                .is_some_and(|after| now < *after)
            {
                counts.backoff += 1;
                continue;
            }
            candidates.push(reference);
        }
        if candidates.is_empty() {
            return;
        }
        // **Permission first, then bytes.** One read of the Workspace's blob
        // rows under the Store lock; the disk is asked afterwards, without it.
        let rows: HashMap<String, (u64, String)> = match self.state.upgrade() {
            Some(state) => match state.store.lock() {
                Ok(mut store) => store
                    .blobs(workspace)
                    .map(|blobs| {
                        blobs
                            .into_iter()
                            .map(|b| (b.hash.as_str().to_owned(), (b.size, b.mime)))
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(_) => HashMap::new(),
            },
            None => HashMap::new(),
        };
        for reference in candidates {
            let Some((size, row_mime)) = rows.get(&reference.blob) else {
                counts.not_here += 1;
                continue;
            };
            let mime = reference.mime.clone().unwrap_or_else(|| row_mime.clone());
            match readable(&mime) {
                Some(Readable::Text) => {}
                Some(Readable::Picture) => {
                    counts.needs_reader += 1;
                    continue;
                }
                None => continue,
            }
            if !self.objects.here(&reference.blob) {
                counts.not_here += 1;
                continue;
            }
            out.push(Pair {
                workspace,
                parent: reference.document,
                hash: reference.blob,
                mime,
                size: *size,
                fresh,
            });
        }
    }

    // ------------------------------------------------------------ one pair

    fn process(&mut self, pair: &Pair, now: i64) -> Processed {
        let Some(state) = self.state.upgrade() else {
            return Processed::Done;
        };
        // Still live, still holding the blob: the plan may be a moment old.
        let live = self
            .projections
            .readable_references(pair.workspace, ReadableScope::Documents(&[pair.parent]))
            .map(|references| references.iter().any(|r| r.blob == pair.hash && !r.trashed))
            .unwrap_or(false);
        if !live {
            self.stats.skipped += 1;
            return Processed::Done;
        }
        let reading_id = reading_document_id(pair.parent);
        // The document itself is the authority; the mirror may lag it.
        let existing = fork(&state, pair.workspace, reading_id)
            .ok()
            .map(ReadingDoc::from_doc);
        let prior = existing.as_ref().and_then(|doc| doc.reading(&pair.hash));
        if prior
            .as_ref()
            .is_some_and(Reading::is_settled_for_this_build)
        {
            self.stats.skipped += 1;
            return Processed::Done;
        }

        // **Copy, don't read again**, when this store root already holds the
        // words for these bytes (TECHNICAL §9).
        let words_here = self
            .written
            .get(&pair.hash)
            .copied()
            .filter(|(_, parent)| *parent != pair.parent)
            .or_else(|| {
                self.mirror
                    .settled_anywhere(&pair.hash, pair.parent)
                    .ok()
                    .flatten()
            });
        if let Some((workspace, parent)) = words_here
            && let Ok(source) = fork(&state, workspace, reading_document_id(parent))
            && let Some(reading) = ReadingDoc::from_doc(source).reading(&pair.hash)
            && reading.is_settled_for_this_build()
        {
            self.set_stage(0, "copying", 1, 0);
            let outcome = self.copy(&state, pair, &reading);
            self.clear_stage();
            if let Err(why) = outcome {
                self.commit_failed(pair, now, why);
            }
            return Processed::Done;
        }

        if let Availability::Unavailable(why) = self.launcher.availability() {
            return Processed::NoReader(why);
        }
        self.worker_down = None;
        if let Err(why) = self.register(&state, pair) {
            self.commit_failed(pair, now, why);
            return Processed::Done;
        }
        let Ok(base) = fork(&state, pair.workspace, reading_id) else {
            self.commit_failed(pair, now, "the reading document did not fork".into());
            return Processed::Done;
        };
        drop(state);
        self.read(pair, base, prior, now)
    }

    /// Write a reading this store root already holds into this entry's
    /// reading document, in one change.
    fn copy(
        &mut self,
        state: &Arc<AppState>,
        pair: &Pair,
        reading: &Reading,
    ) -> Result<(), String> {
        self.register(state, pair)?;
        let base = fork(state, pair.workspace, reading_document_id(pair.parent))
            .map_err(|_| "the reading document did not fork".to_owned())?;
        let mut writer = Writer::new(pair.workspace, reading_document_id(pair.parent), base);
        match writer.doc.write(&pair.hash, reading) {
            Ok(Write::Applied) => {}
            Ok(Write::Superseded { .. }) => {
                self.stats.skipped += 1;
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
        writer.commit(state)?;
        self.stats.commits += 1;
        self.stats.copied += 1;
        self.debug_line(0, pair, "copied", reading.pages.len());
        Ok(())
    }

    /// Run the worker over one pair and write what it says.
    fn read(
        &mut self,
        pair: &Pair,
        base: loro::LoroDoc,
        prior: Option<Reading>,
        now: i64,
    ) -> Processed {
        let Some(path) = self.objects.path(&pair.hash) else {
            self.stats.skipped += 1;
            return Processed::Done;
        };
        let reading_id = reading_document_id(pair.parent);
        let mut writer = Writer::new(pair.workspace, reading_id, base);
        // A `partial` with no reason at this generation is a reading that was
        // interrupted: resume after the last page it reached.
        let resume = prior
            .as_ref()
            .filter(|r| {
                r.generation == GENERATION
                    && r.status == Some(ReadingStatus::Partial)
                    && r.reason.is_none()
            })
            .and_then(|r| r.pages_done);
        let from_page = resume.map_or(1, |done| done.saturating_add(1));
        let held_words = prior
            .as_ref()
            .is_some_and(|r| r.generation == GENERATION && r.pages.values().any(|t| !t.is_empty()));
        let mut source = prior
            .as_ref()
            .filter(|r| r.generation == GENERATION)
            .and_then(|r| r.source);
        match writer.doc.begin(&pair.hash, &ReaderId::text(), GENERATION) {
            Ok(Write::Applied) => {}
            Ok(Write::Superseded { .. }) => {
                self.stats.skipped += 1;
                return Processed::Done;
            }
            Err(error) => {
                self.stats.last_error = Some(error.to_string());
                return Processed::Done;
            }
        }
        let job = JobId(self.next_job);
        self.next_job += 1;
        let request = Request::Read {
            job,
            path,
            mime: pair.mime.clone(),
            from_page,
            max_text_pages: Some(TEXT_PAGE_CAP),
            max_read_pages: Some(koru_read::READ_PAGE_CAP),
            want: Want::Text,
        };
        self.set_stage(job.0, "reading", from_page, 0);

        let state = self.state.clone();
        let current = Arc::clone(&self.current);
        let progress = self.on_progress.take();
        let mut buffer: Vec<(u32, PageText)> = Vec::new();
        let mut last_page: Option<u32> = None;
        let mut words = 0usize;
        let mut seen = 0u32;
        let mut terminal: Option<Event> = None;
        let mut write_error: Option<String> = None;
        let mut commits = 0u64;
        let outcome = self.launcher.read(&request, &mut |event| {
            if event.job() != Some(job) || terminal.is_some() {
                return;
            }
            match event {
                Event::Page {
                    page,
                    text,
                    source: from,
                    ..
                } => {
                    if page < from_page {
                        return;
                    }
                    seen += 1;
                    last_page = Some(last_page.map_or(page, |l| l.max(page)));
                    if !text.is_empty() {
                        words += 1;
                        source = Some(source.map_or(from, |s| s.combine(from)));
                        buffer.push((page, text));
                    }
                    if buffer.len() >= FLUSH_PAGES && write_error.is_none() {
                        if let Some(state) = state.upgrade() {
                            match writer.put(&pair.hash, &mut buffer, last_page) {
                                Ok(()) => match writer.commit(&state) {
                                    Ok(()) => commits += 1,
                                    Err(why) => write_error = Some(why),
                                },
                                Err(why) => write_error = Some(why),
                            }
                        }
                        if let Ok(mut current) = current.lock()
                            && let Some(current) = current.as_mut()
                        {
                            current.page = page;
                        }
                        if let Some(progress) = progress.as_ref() {
                            progress();
                        }
                    }
                }
                end @ (Event::Done { .. } | Event::Failed { .. }) => terminal = Some(end),
                Event::Status(_) => {}
            }
        });
        self.on_progress = progress;
        self.stats.commits += commits;
        self.clear_stage();
        let Some(state) = self.state.upgrade() else {
            return Processed::Done;
        };
        if let Some(why) = write_error {
            self.commit_failed(pair, now, why);
            return Processed::Done;
        }
        self.stats.pages += u64::from(seen);

        let result: Result<&'static str, String> = match (outcome, terminal) {
            (
                Ok(()),
                Some(Event::Done {
                    status,
                    reason,
                    pages_done,
                    ..
                }),
            ) => {
                // On a resume, `empty` describes only this job's pages.
                let status = if status == ReadingStatus::Empty && (held_words || words > 0) {
                    ReadingStatus::Read
                } else {
                    status
                };
                let pages_done = pages_done.max(last_page.unwrap_or(0));
                let finished = writer
                    .put(&pair.hash, &mut buffer, Some(pages_done))
                    .and_then(|()| {
                        let outcome = Outcome {
                            status,
                            reason,
                            source,
                            confidence: None,
                            pages_done: (status == ReadingStatus::Partial).then_some(pages_done),
                        };
                        writer
                            .doc
                            .finish(&pair.hash, GENERATION, &outcome)
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|()| writer.commit(&state));
                finished.map(|()| {
                    self.stats.commits += 1;
                    if status.is_finished(reason) {
                        self.written
                            .insert(pair.hash.clone(), (pair.workspace, pair.parent));
                    }
                    match status {
                        ReadingStatus::Read => self.stats.read += 1,
                        ReadingStatus::Empty => self.stats.empty += 1,
                        ReadingStatus::Partial => self.stats.partial += 1,
                        ReadingStatus::Failed => self.stats.failed += 1,
                    }
                    if status == ReadingStatus::Partial && reason.is_none() {
                        // Pages that need the reader: wait as `needs_reader`,
                        // not fed again until one is installed.
                        self.record(&pair.hash, AttemptCode::NeedsReader, now);
                    } else if let Err(error) = self.mirror.clear_attempts(&pair.hash) {
                        self.stats.last_error = Some(error.0);
                    }
                    status.as_str()
                })
            }
            (Ok(()), Some(Event::Failed { code, .. })) => {
                let kept = self.keep_progress(&state, &mut writer, pair, &mut buffer, last_page);
                match local_code(code) {
                    Some(attempt) => {
                        if attempt != AttemptCode::NeedsReader {
                            self.stats.failed += 1;
                            *self
                                .stats
                                .failed_by_code
                                .entry(attempt.as_str().to_owned())
                                .or_default() += 1;
                        }
                        self.record(&pair.hash, attempt, now);
                    }
                    // Busy or cancelled: about the conversation, not the file.
                    None => {
                        self.retry_after.insert(
                            (pair.parent, pair.hash.clone()),
                            now.saturating_add(BACKOFF_MILLIS),
                        );
                    }
                }
                kept.map(|()| code.as_str())
            }
            (Err(Interrupted::Unavailable(why)), _) => {
                return Processed::NoReader(why);
            }
            (Err(Interrupted::Crashed { memory_limit }), _) => {
                // The page after the last one delivered is the one it died on:
                // skip it on the retry (resume at last page + 2).
                let stuck = last_page.map_or(from_page, |l| l + 1);
                let kept = self.keep_progress(&state, &mut writer, pair, &mut buffer, Some(stuck));
                let code = if memory_limit {
                    AttemptCode::MemoryLimit
                } else {
                    AttemptCode::Crashed
                };
                self.stats.failed += 1;
                *self
                    .stats
                    .failed_by_code
                    .entry(code.as_str().to_owned())
                    .or_default() += 1;
                self.record(&pair.hash, code, now);
                kept.map(|()| code.as_str())
            }
            (Err(Interrupted::Hostile), _) => {
                let kept = self.keep_progress(&state, &mut writer, pair, &mut buffer, last_page);
                self.stats.failed += 1;
                *self
                    .stats
                    .failed_by_code
                    .entry(AttemptCode::Hostile.as_str().to_owned())
                    .or_default() += 1;
                self.record(&pair.hash, AttemptCode::Hostile, now);
                kept.map(|()| AttemptCode::Hostile.as_str())
            }
            // `Ok` with no terminal event breaks the launcher's own promise;
            // treat it as the worker ending mid-job.
            (Ok(()), _) => {
                self.record(&pair.hash, AttemptCode::Crashed, now);
                Ok(AttemptCode::Crashed.as_str())
            }
        };
        match result {
            Ok(outcome) => self.debug_line(job.0, pair, outcome, words),
            Err(why) => self.commit_failed(pair, now, why),
        }
        Processed::Done
    }

    /// The words that did arrive before a job ended badly are real words:
    /// write them, with how far the reading got, so the entry resumes there.
    fn keep_progress(
        &mut self,
        state: &Arc<AppState>,
        writer: &mut Writer,
        pair: &Pair,
        buffer: &mut Vec<(u32, PageText)>,
        reached: Option<u32>,
    ) -> Result<(), String> {
        if buffer.is_empty() && reached.is_none() && !writer.wrote {
            return Ok(());
        }
        writer.put(&pair.hash, buffer, reached)?;
        writer.commit(state)?;
        self.stats.commits += 1;
        Ok(())
    }

    /// Make sure the entry's reading document is registered and has storage.
    ///
    /// **A fresh file registers alone and at once; a backlog registers in
    /// batches** (TECHNICAL §9): the next unregistered parents of the backlog
    /// in the same workspace are registered with this one, in one manifest
    /// commit.
    fn register(&mut self, state: &Arc<AppState>, pair: &Pair) -> Result<(), String> {
        let mut store = state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned".to_owned())?;
        let registered = |store: &crate::WorkerStore, parent: DocumentId| {
            store.document_kind(pair.workspace, reading_document_id(parent))
                == Some(KIND_READING.to_owned())
        };
        let mut deltas = Vec::new();
        if !pair.fresh && !registered(&store, pair.parent) {
            let mut parents = vec![pair.parent];
            parents.extend(
                self.plan
                    .iter()
                    .filter(|p| !p.fresh && p.workspace == pair.workspace)
                    .map(|p| p.parent)
                    .filter(|parent| !registered(&store, *parent))
                    .take(REGISTER_BATCH - 1),
            );
            let batch = store
                .ensure_readings(pair.workspace, &parents, Timestamp::now())
                .map_err(|e| format!("{:?}", e.code()))?;
            self.stats.registered += batch.created.len() as u64;
            if !batch.delta.is_empty() {
                deltas.push(batch.delta);
            }
        }
        let one = store
            .ensure_reading(pair.workspace, pair.parent, Timestamp::now())
            .map_err(|e| format!("{:?}", e.code()))?;
        if one.created {
            self.stats.registered += 1;
            deltas.push(one.delta);
        }
        drop(store);
        for delta in deltas {
            state.broadcast_manifest(pair.workspace, delta);
            state.notify_replication();
        }
        Ok(())
    }

    fn record(&mut self, hash: &str, code: AttemptCode, now: i64) {
        if let Err(error) = self.mirror.record_attempt(hash, READER_TEXT, code, now) {
            self.stats.last_error = Some(error.0);
        }
    }

    /// A commit failed — a full disk, a poisoned lock. Nothing is lost: the
    /// pair is retried after a backoff, in memory (TECHNICAL §11).
    fn commit_failed(&mut self, pair: &Pair, now: i64, why: String) {
        if self.stats.last_error.as_deref() != Some(why.as_str()) {
            tracing::warn!(target: "koru_readings", "a reading could not be written: {why}");
        }
        self.stats.last_error = Some(why);
        self.retry_after.insert(
            (pair.parent, pair.hash.clone()),
            now.saturating_add(BACKOFF_MILLIS),
        );
    }

    fn set_stage(&mut self, job: u64, stage: &'static str, from_page: u32, page: u32) {
        if let Ok(mut current) = self.current.lock() {
            *current = Some(CurrentJob {
                job,
                stage,
                from_page,
                page,
            });
        }
        if let Some(progress) = self.on_progress.as_ref() {
            progress();
        }
    }

    fn clear_stage(&mut self) {
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }

    /// One line per job, behind `KORU_READ_DEBUG` (TECHNICAL §14). Ids, an
    /// outcome token and a count — never a hash, a name or a word.
    fn debug_line(&self, job: u64, pair: &Pair, outcome: &str, pages_with_words: usize) {
        if self.debug {
            tracing::info!(
                target: "koru_readings",
                job,
                workspace = %pair.workspace,
                document = %pair.parent,
                fresh = pair.fresh,
                outcome,
                pages_with_words,
                "reading job"
            );
        }
    }
}

/// Fork one document under the Store lock. In, fork, out.
fn fork(
    state: &Arc<AppState>,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Result<loro::LoroDoc, String> {
    let mut store = state
        .store
        .lock()
        .map_err(|_| "the store lock is poisoned".to_owned())?;
    store
        .fork_document(workspace, document)
        .map_err(|e| format!("{:?}", e.code()))
}

/// One job's writes into one reading document: a fork, the changes made to it
/// locally, and the version last sent to the Store, so each commit carries
/// only what this job wrote since the one before.
struct Writer {
    workspace: WorkspaceId,
    document: DocumentId,
    doc: ReadingDoc,
    sent: loro::VersionVector,
    wrote: bool,
}

impl Writer {
    fn new(workspace: WorkspaceId, document: DocumentId, base: loro::LoroDoc) -> Self {
        let sent = base.oplog_vv();
        Self {
            workspace,
            document,
            doc: ReadingDoc::from_doc(base),
            sent,
            wrote: false,
        }
    }

    /// Write the buffered pages, and how far the reading has got.
    fn put(
        &mut self,
        hash: &str,
        buffer: &mut Vec<(u32, PageText)>,
        pages_done: Option<u32>,
    ) -> Result<(), String> {
        let pages: Vec<(u32, &PageText)> = buffer.iter().map(|(n, t)| (*n, t)).collect();
        if pages.is_empty() && pages_done.is_none() {
            return Ok(());
        }
        self.doc
            .put_pages(hash, GENERATION, &pages, pages_done)
            .map_err(|e| e.to_string())?;
        buffer.clear();
        Ok(())
    }

    /// Apply what was written since the last commit through the Store's
    /// ordinary update path, with the Store as its origin, flushed like any
    /// commit; then tell the document's room and the peers, after the flush.
    fn commit(&mut self, state: &Arc<AppState>) -> Result<(), String> {
        let delta = self
            .doc
            .export_updates_since(&self.sent)
            .map_err(|e| e.to_string())?;
        let now = self.doc.version();
        if now == self.sent {
            return Ok(());
        }
        let at = Timestamp::now();
        let committed = {
            let mut store = state
                .store
                .lock()
                .map_err(|_| "the store lock is poisoned".to_owned())?;
            store
                .apply_update(
                    self.workspace,
                    self.document,
                    delta.clone(),
                    at,
                    UpdateOrigin::Store,
                )
                .and_then(|()| store.flush(self.workspace, self.document, at))
                .map_err(|e| format!("{:?}", e.code()))?
        };
        self.sent = now;
        self.wrote = true;
        if committed.is_some() {
            state.broadcast_document(self.document, delta);
            state.notify_replication();
        }
        Ok(())
    }
}

/// What a local attempt means for the queue now.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Go,
    NeedsReader,
    Backoff,
    Failed,
}

/// The attempts table's policy (TECHNICAL §11): a file that needs the reader
/// waits for one; a format this computer cannot decode and a hostile file are
/// left until the next generation; a crash or a memory kill gets one retry in
/// a fresh worker; an unreadable object or an internal error is retried with
/// backoff, a few times.
pub fn verdict(attempt: &Attempt, now: i64) -> Verdict {
    match attempt.code {
        AttemptCode::NeedsReader => Verdict::NeedsReader,
        AttemptCode::Unsupported | AttemptCode::Hostile => Verdict::Failed,
        AttemptCode::Crashed | AttemptCode::MemoryLimit => {
            if attempt.attempts >= 2 {
                Verdict::Failed
            } else {
                Verdict::Go
            }
        }
        AttemptCode::Unreadable | AttemptCode::Internal => {
            if attempt.attempts >= MAX_RETRIES {
                return Verdict::Failed;
            }
            let shift = attempt.attempts.saturating_sub(1).min(8);
            let wait = BACKOFF_MILLIS
                .saturating_mul(1_i64 << (2 * shift))
                .min(BACKOFF_CEILING_MILLIS);
            if now < attempt.last_at.saturating_add(wait) {
                Verdict::Backoff
            } else {
                Verdict::Go
            }
        }
    }
}

/// The worker's failure codes the attempts table remembers.
fn local_code(code: FailureCode) -> Option<AttemptCode> {
    match code {
        FailureCode::Unsupported => Some(AttemptCode::Unsupported),
        FailureCode::Unreadable => Some(AttemptCode::Unreadable),
        FailureCode::NeedsReader => Some(AttemptCode::NeedsReader),
        FailureCode::Internal => Some(AttemptCode::Internal),
        FailureCode::Busy | FailureCode::Cancelled => None,
    }
}

#[cfg(test)]
mod tests;
