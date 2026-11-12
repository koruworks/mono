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
//! - the blob is readable: a PDF or a plain-text file always; a picture, or
//!   a PDF whose text-only pass left pages for the reader, only while the
//!   reader is installed **and the worker's own status says it recognizes**
//!   (OCR Plan 2.4). Otherwise it is counted `needs_reader` and not sent;
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
//! A settled entry is queued again for two reasons only: another entry on this
//! store root holds a **fuller** reading of the same bytes (a capped
//! `partial` read to the end, a higher generation), which is copied to it —
//! never read (TECHNICAL §18, the review's note; OCR Plan 3.1); or **a person
//! asked** for it (below).
//!
//! ## What a person asked for (OCR Plan 3.2; TECHNICAL D12, §9)
//!
//! *Read all pages* and *Read again* are rows of the mirror's
//! `read_requests`, one per workspace and hash. A requested item is queued
//! **behind anything freshly added and ahead of the backlog**, in the order
//! the requests were made, and one entry per requested hash carries it (the
//! one already being read, else the first): the others holding the same
//! bytes receive the result by the copy rule above.
//!
//! - A PDF is read **in windows**: from where its reading stands (`pages_done`
//!   + 1, or page 1 for *Read again* on a finished reading), at most
//!   [`REQUEST_READ_WINDOW`] page sent to the reader and [`REQUEST_TEXT_WINDOW`]
//!   text-layer pages per job. The worker ends a window as it ends any capped
//!   job, `partial/page-cap`; for a requested PDF that means *the window is
//!   done*, and the coordinator writes it as `partial` with no reason and puts
//!   the item back at the end of the requested lane. **That is how it yields
//!   between pages**: a file added meanwhile is in the fresh lane, ahead of
//!   the next window. No contract change: `from_page` and the two caps were
//!   always per job.
//! - The last window writes `read` (or `empty`, `failed`), and the request is
//!   met. So is one whose item got a fuller reading by copy, or whose item is
//!   gone.
//! - A picture or a plain-text file is one page: one ordinary job, from the
//!   start.
//!
//! A request never pauses for battery saver: a person asked for it.
//!
//! ## Out of the way (TECHNICAL §14, OCR Plan 3.1)
//!
//! The backlog waits while the system saves energy ([`super::power`]); a
//! fresh file does not. The worker is launched below normal priority with
//! background I/O (`supervised.rs`).
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
use koru_models::ModelLease;
use koru_projection::{Projections, ReadableReference, ReadableScope};
use koru_read::{
    Event, FailureCode, GENERATION, JobId, Outcome, PageText, READER_TEXT, ReaderId, Reading,
    ReadingDoc, ReadingStatus, Reason, Request, TEXT_PAGE_CAP, Want, Write,
};
use koru_store_api::state::AppState;
use koru_store_core::ids::{DocumentId, WorkspaceId, reading_document_id};
use koru_store_core::model::Timestamp;
use koru_store_core::ports::UpdateOrigin;

use koru_store_wire::reader::MachineReadingCounts;

use super::feed::{CoordinatorWork, Inbox};
use super::hub::{ModelInUse, ReaderHub, ScopeCounts, WorkerKnowledge};
use super::launcher::{Availability, Interrupted, Launcher, Unavailable};
use super::mirror::{Attempt, AttemptCode, Mirror, Rank, Settled, rank_of};
use super::power::{NeverSaving, Power};

/// While a queue is running, a listing change recomputes it at most this
/// often. New files reach the fresh lane at once regardless; this only bounds
/// how often the backlog's order and counts are redone during an import.
pub const RECOMPUTE_EVERY_MILLIS: i64 = 2_000;

/// At most this many reading documents are registered in one manifest commit
/// (TECHNICAL §9 "a few hundred at a time"). **The boundary is a count and
/// nothing else**: a batch is whatever the queue holds unregistered in the
/// pair's workspace when the pair is reached, so a file on its own is a batch
/// of one, registered at once, and nothing ever waits for a batch to fill.
pub const REGISTER_BATCH: usize = 200;

/// Pages written into a reading document per Store commit while a long file
/// is read. Plan 0.1 measured a text layer at 182–440 pages a second, so the
/// commits are what costs, and a typical file (two pages) is one commit.
pub const FLUSH_PAGES: usize = 64;

/// **Pages sent to the reader per job while a person's request is met**
/// (OCR Plan 3.2): one, so a file added while a long scan is read in full is
/// read before the scan's next page (TECHNICAL §9 "It yields between pages").
/// A window costs a worker round trip and one commit; a recognized page costs
/// about a second, so the commit is the small part.
pub const REQUEST_READ_WINDOW: u32 = 1;

/// **Text-layer pages per job while a request is met.** Plan 0.1 measured
/// 182–440 text pages a second, so a window is about a second of work, the
/// same yield as one recognized page.
pub const REQUEST_TEXT_WINDOW: u32 = 200;

/// Local failures that are retried at all, before the file is left until the
/// next generation.
const MAX_RETRIES: u32 = 3;

/// A model the worker could not load this many times is left alone until it
/// is installed again: each try costs a worker start and a failed load.
const LOAD_FAILURES_TO_GIVE_UP: u32 = 2;

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
    /// Pixels: needs the reader.
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
pub trait Objects: Send + Sync {
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
    /// `Recognize` only when the worker said it recognizes (OCR Plan 2.4).
    pub want: Want,
    /// Read from page 1 whatever a `partial` says: the earlier pass was a
    /// text-only one that left pages for the reader, and this is the reader.
    pub restart: bool,
    /// In the counts the last recompute published, so finishing it moves
    /// them. A fresh pair is counted by the recompute it makes owed.
    pub counted: bool,
    /// A person asked for it: *Read all pages* or *Read again* (OCR Plan
    /// 3.2). Queued behind the fresh pairs and ahead of the backlog; never
    /// also `fresh`.
    pub requested: bool,
}

pub(crate) fn is_pdf(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/pdf"))
}

/// One recompute's counts: the debug route's, each workspace's, this
/// computer's.
#[derive(Default)]
struct Tally {
    queue: QueueCounts,
    per_workspace: HashMap<WorkspaceId, ScopeCounts>,
    machine: MachineReadingCounts,
}

impl Tally {
    fn settled(&mut self, workspace: WorkspaceId, settled: &Settled) {
        self.per_workspace.entry(workspace).or_default().total += 1;
        self.machine.total += 1;
        match settled {
            Settled::Read => self.machine.read += 1,
            Settled::PartlyRead => self.machine.partly_read += 1,
            Settled::Failed(reason) => self.failed(reason),
        }
    }

    /// Given up on here (a local failure past its retries): in the total,
    /// not unread.
    fn failed_here(&mut self, workspace: WorkspaceId, code: &str) {
        self.per_workspace.entry(workspace).or_default().total += 1;
        self.machine.total += 1;
        self.failed(code);
    }

    fn failed(&mut self, reason: &str) {
        self.machine.could_not_be_read += 1;
        *self
            .machine
            .could_not_be_read_by_reason
            .entry(reason.to_owned())
            .or_default() += 1;
    }

    /// An item with no finished reading that is not given up on.
    fn unread(&mut self, workspace: WorkspaceId) -> &mut ScopeCounts {
        self.machine.total += 1;
        self.machine.unread += 1;
        let scope = self.per_workspace.entry(workspace).or_default();
        scope.total += 1;
        scope.unread += 1;
        scope
    }

    fn needs_reader(&mut self, workspace: WorkspaceId) {
        self.queue.needs_reader += 1;
        self.machine.needs_reader += 1;
        self.unread(workspace).needs_reader += 1;
    }

    fn not_here(&mut self, workspace: WorkspaceId) {
        self.queue.not_here += 1;
        self.machine.not_here += 1;
        self.unread(workspace).not_here += 1;
    }
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
    /// Asked for by a person, between the fresh pairs and the backlog.
    pub requested: u64,
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
    /// Of `copied`: a settled entry given the fuller reading another entry
    /// holds of the same bytes (TECHNICAL §18, the review's note).
    pub refreshed: u64,
    pub skipped: u64,
    pub pages: u64,
    /// Windows of a requested PDF written as `partial` with no reason, each
    /// followed by a yield to whatever was added meanwhile (OCR Plan 3.2).
    pub windows: u64,
    /// Requests met: read to the end, read again, or given a fuller reading.
    pub requests_met: u64,
    pub registered: u64,
    /// Manifest commits made registering reading documents — what batching
    /// keeps to a handful during a backlog (TECHNICAL §9).
    pub manifest_commits: u64,
    /// Commits to reading documents.
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
    /// Shared with the readings route, which asks whether the bytes are here
    /// before it accepts a request (OCR Plan 3.2).
    objects: Arc<dyn Objects>,
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
    /// Readings this coordinator finished while the queue ran, by hash: the
    /// copy rule's answer for bytes the mirror has not applied yet — a
    /// backlog holding the same picture twice in a row would otherwise read it
    /// twice. **Kept until the queue empties, not cleared by a recompute**:
    /// the mirror applies on its own thread, and a recompute between two
    /// pairs of the same bytes, before the mirror held the first, read the
    /// second again (OCR Plan 3.1's real run: 2 of 46). An entry whose
    /// document is gone simply does not fork, and the mirror is asked too.
    written: HashMap<String, (WorkspaceId, DocumentId)>,
    next_job: u64,
    backlog_seen: u64,
    pub stats: CoordinatorStats,
    pub current: Current,
    pub worker_down: Option<Unavailable>,
    debug: bool,
    on_progress: Option<Box<dyn Fn() + Send>>,
    // ---- the reader (OCR Plan 2.4)
    /// Where counts go and the installed model comes from. `None` on a host
    /// with no installer: nothing is recognized and nothing is offered.
    hub: Option<Arc<ReaderHub>>,
    /// The model the worker is launched with, and the lease that keeps it.
    model: Option<ModelInUse>,
    _lease: Option<Box<dyn ModelLease>>,
    model_revision: u64,
    /// What the worker's status said about its build, once asked.
    built: Option<bool>,
    worker_missing: bool,
    /// The launch on which the worker answered a recognition job
    /// `needs-reader`: its model would not load, so recognition is off until
    /// a worker of another launch says otherwise.
    lost_on: Option<Option<String>>,
    load_failures: u32,
    per_workspace: HashMap<WorkspaceId, ScopeCounts>,
    machine: MachineReadingCounts,
    /// How the pair just processed ended, when it settled, for the counts.
    settled_last: Option<Settled>,
    /// The counts are owed a recompute, **at the recompute cadence and never
    /// sooner**: a pair skipped because the mirror lags the document must
    /// not turn every step into a full recompute that queues it again.
    recount: bool,
    // ---- staying out of the way (OCR Plan 3.1)
    /// Whether the system is saving energy. [`NeverSaving`] unless the daemon
    /// hands the real one in ([`Self::with_power`]).
    power: Box<dyn Power>,
    /// The backlog is waiting out battery saver; fresh files still go.
    pub paused: bool,
    /// The best settled reading of each hash more than one entry holds, as of
    /// the last recompute: a settled entry ranked below it is given it.
    fuller: HashMap<String, Rank>,
}

impl Coordinator {
    /// A coordinator over objects of its own (the tests' composition).
    #[cfg(test)]
    pub fn new(
        state: &Arc<AppState>,
        projections: Projections,
        mirror: Mirror,
        launcher: Box<dyn Launcher>,
        objects: Box<dyn Objects>,
        inbox: Arc<Inbox>,
    ) -> Self {
        Self::shared(
            state,
            projections,
            mirror,
            launcher,
            Arc::from(objects),
            inbox,
        )
    }

    /// [`Self::new`], with the objects shared with the readings route.
    pub fn shared(
        state: &Arc<AppState>,
        projections: Projections,
        mirror: Mirror,
        launcher: Box<dyn Launcher>,
        objects: Arc<dyn Objects>,
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
            hub: None,
            model: None,
            _lease: None,
            model_revision: 0,
            built: None,
            worker_missing: false,
            lost_on: None,
            load_failures: 0,
            per_workspace: HashMap::new(),
            machine: MachineReadingCounts::default(),
            settled_last: None,
            recount: false,
            power: Box::new(NeverSaving),
            paused: false,
            fuller: HashMap::new(),
        }
    }

    /// **Pause the backlog while the system saves energy** (TECHNICAL §14):
    /// the daemon hands in the operating system's answer; every other
    /// composition, tests included, keeps [`NeverSaving`].
    pub fn with_power(mut self, power: Box<dyn Power>) -> Self {
        self.power = power;
        self
    }

    /// **The reader**: publish counts and follow the installed model through
    /// `hub` (OCR Plan 2.4).
    pub fn with_reader(mut self, hub: Arc<ReaderHub>) -> Self {
        hub.attach_coordinator();
        self.hub = Some(hub);
        self
    }

    // ----------------------------------------------------------- the reader

    /// **Follow the installed model.** A change — an install, a removal —
    /// lets the running worker go (the next starts with or without
    /// `--model`), drops or takes the lease, and makes the whole queue owed:
    /// what waited for the reader is queued now, or waits again.
    fn follow_model(&mut self) {
        let Some(hub) = self.hub.clone() else {
            return;
        };
        let (revision, wanted) = hub.wanted_model();
        if revision == self.model_revision {
            return;
        }
        self.model_revision = revision;
        self._lease = None;
        self.model = None;
        if let Some(model) = wanted
            && let Some(lease) = hub.lease(&model.installed)
        {
            self._lease = lease;
            self.model = Some(model);
        }
        self.launcher
            .set_model(self.model.as_ref().map(ModelInUse::dir));
        self.lost_on = None;
        self.load_failures = 0;
        self.owed = true;
        hub.ack_model(revision);
        self.publish();
    }

    /// Ask the worker what it is, with no job — before offering a download.
    fn learn_build(&mut self) {
        match self.launcher.probe() {
            Ok(status) => {
                self.built = Some(status.recognition_built);
                self.worker_missing = false;
            }
            Err(Unavailable::NoLauncher | Unavailable::NotInstalled) => {
                self.worker_missing = true;
            }
            Err(_) => {}
        }
    }

    fn will_not_load(&self) -> bool {
        self.load_failures >= LOAD_FAILURES_TO_GIVE_UP
    }

    /// **Whether the worker recognizes now**: a model is installed and leased,
    /// and the worker's first status on its current launch said
    /// `recognition: true`. Asks the worker (starting it) when it has not
    /// been asked since it started — only ever when something is waiting to
    /// be recognized.
    fn recognizing(&mut self) -> bool {
        if self.model.is_none() || self.will_not_load() {
            return false;
        }
        let status = match self.launcher.status() {
            Some(status) => status,
            None => match self.launcher.probe() {
                Ok(status) => status,
                Err(Unavailable::NoLauncher | Unavailable::NotInstalled) => {
                    self.worker_missing = true;
                    return false;
                }
                Err(_) => return false,
            },
        };
        self.built = Some(status.recognition_built);
        if self.lost_on.as_ref() == Some(&status.supervisor_launch_id) {
            return false;
        }
        status.recognition
    }

    /// Hand the counts and what is known of the worker to the hub.
    fn publish(&mut self) {
        let Some(hub) = self.hub.clone() else {
            return;
        };
        let recognizing = self.model.is_some()
            && !self.will_not_load()
            && self.launcher.status().is_some_and(|status| {
                status.recognition && self.lost_on.as_ref() != Some(&status.supervisor_launch_id)
            });
        hub.publish_worker(WorkerKnowledge {
            built: self.built,
            missing: self.worker_missing,
            recognizing,
            will_not_load: self.model.is_some() && self.will_not_load(),
        });
        if self.computed {
            hub.publish_counts(self.per_workspace.clone(), self.machine.clone());
        }
    }

    /// **The worker could not load the model**: it answered a recognition job
    /// `needs-reader`. Recognition is off for this launch of the worker; a
    /// later launch tries once more, and after
    /// [`LOAD_FAILURES_TO_GIVE_UP`] the model is reported as one that will
    /// not load until it is installed again.
    fn recognition_lost(&mut self) {
        self.lost_on = Some(
            self.launcher
                .status()
                .and_then(|status| status.supervisor_launch_id),
        );
        self.load_failures += 1;
        self.recount = true;
        tracing::warn!(
            target: "koru_readings",
            failures = self.load_failures,
            "the read worker could not use the installed reader; recognition waits"
        );
    }

    /// A counted pair settled: it leaves `unread` for its category, so the
    /// number in search falls as a backlog is read.
    fn count_settled(&mut self, pair: &Pair, settled: &Settled) {
        if !pair.counted {
            return;
        }
        let scope = self.per_workspace.entry(pair.workspace).or_default();
        scope.unread = scope.unread.saturating_sub(1);
        self.machine.unread = self.machine.unread.saturating_sub(1);
        match settled {
            Settled::Read => self.machine.read += 1,
            Settled::PartlyRead => self.machine.partly_read += 1,
            Settled::Failed(reason) => {
                self.machine.could_not_be_read += 1;
                *self
                    .machine
                    .could_not_be_read_by_reason
                    .entry(reason.clone())
                    .or_default() += 1;
            }
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
        self.follow_model();
        if self.hub.as_ref().is_some_and(|hub| hub.take_probe_wanted())
            && self.built.is_none()
            && !self.worker_missing
        {
            self.learn_build();
            self.publish();
        }
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
        if self.recount && now.saturating_sub(self.last_recompute) >= RECOMPUTE_EVERY_MILLIS {
            self.owed = true;
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
        // **A backlog pauses under battery saver; a fresh file does not**
        // (TECHNICAL §14). Fresh pairs are always at the front, so only a
        // backlog pair at the head asks. Paused, this step does nothing and
        // the thread waits as it does when idle: a trigger or the linger
        // brings it back to ask again.
        // A person's request does not wait either: they asked for it.
        match self.plan.front() {
            Some(next) if !next.fresh && !next.requested => {
                let saving = self.power.saving();
                self.set_paused(saving);
                if saving {
                    self.publish();
                    return false;
                }
            }
            Some(_) => {}
            None => self.set_paused(false),
        }
        let Some(pair) = self.plan.pop_front() else {
            self.written.clear();
            self.finish_backlog();
            self.publish();
            return false;
        };
        if pair.fresh {
            self.counts.fresh = self.counts.fresh.saturating_sub(1);
        } else if pair.requested {
            self.counts.requested = self.counts.requested.saturating_sub(1);
        } else {
            self.counts.backlog = self.counts.backlog.saturating_sub(1);
            self.backlog_seen += 1;
        }
        self.settled_last = None;
        match self.process(&pair, now) {
            Processed::Done => {
                if pair.requested {
                    // Counted as settled (or unread, mid-window) by the last
                    // recompute: the next one, at its cadence, moves it.
                    self.settled_last = None;
                    self.recount = true;
                } else if let Some(settled) = self.settled_last.take() {
                    self.count_settled(&pair, &settled);
                } else {
                    // Waiting, failed here, or skipped: the recompute says
                    // which, at its cadence.
                    self.recount = true;
                }
            }
            Processed::NoReader(why) => {
                self.worker_down = Some(why);
                self.counts.waiting_for_reader += 1;
                self.waiting.push(pair);
            }
        }
        self.publish();
        true
    }

    /// The backlog's pause, said once each way: news, not activity.
    fn set_paused(&mut self, paused: bool) {
        if paused == self.paused {
            return;
        }
        self.paused = paused;
        if paused {
            tracing::info!(
                target: "koru_readings",
                queued = self.plan.len(),
                "battery saver is on: the reading backlog waits; new files are still read"
            );
        } else {
            tracing::info!(target: "koru_readings", "the reading backlog goes on");
        }
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
        self.recount = false;
        self.computed = true;
        self.last_recompute = now;
        let mut tally = Tally::default();
        let mut recognize: Option<bool> = None;
        let mut plan = Vec::new();
        let attempts = self.mirror.attempts(READER_TEXT).unwrap_or_default();
        self.fuller = self.mirror.best_shared().unwrap_or_default();
        // Every item the listing still holds, trashed or not: a request for
        // anything else is for an item that is gone (OCR Plan 3.2).
        let mut held: HashSet<(WorkspaceId, String)> = HashSet::new();
        let mut complete = true;
        let workspaces = match self.projections.listings() {
            Ok(listings) => listings.into_iter().map(|(w, _)| w).collect::<Vec<_>>(),
            Err(error) => {
                self.stats.last_error = Some(error.0);
                complete = false;
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
                    complete = false;
                    continue;
                }
            };
            held.extend(references.iter().map(|r| (workspace, r.blob.clone())));
            self.classify(
                workspace,
                references,
                false,
                &attempts,
                now,
                &mut plan,
                &mut tally,
                &mut recognize,
            );
        }
        if complete {
            self.forget_requests_for_gone_items(&held);
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
            pair.fresh = !pair.requested && fresh.contains(&(pair.parent, pair.hash.clone()));
        }
        // Fresh, then what a person asked for in the order they asked, then
        // the backlog smallest first (OCR Plan 3.2).
        let order: HashMap<(WorkspaceId, String), usize> = self
            .requests_in_order()
            .into_iter()
            .enumerate()
            .map(|(at, request)| (request, at))
            .collect();
        plan.sort_by_key(|p| {
            let asked = p
                .requested
                .then(|| order.get(&(p.workspace, p.hash.clone())).copied())
                .flatten()
                .unwrap_or(usize::MAX);
            (!p.fresh, !p.requested, asked, p.key())
        });
        let Tally {
            queue: mut counts,
            per_workspace,
            machine,
        } = tally;
        counts.fresh = plan.iter().filter(|p| p.fresh).count() as u64;
        counts.requested = plan.iter().filter(|p| p.requested).count() as u64;
        counts.backlog = plan.len() as u64 - counts.fresh - counts.requested;
        // What is waiting for a reader is in the plan again; it waits again
        // the moment it is reached if none is back.
        self.waiting.clear();
        self.plan = plan.into();
        self.counts = counts;
        self.stats.recomputes += 1;
        self.stats.last_recompute_millis = started.elapsed().as_millis() as u64;
        // **Before offering a download, learn whether this worker could use
        // it** (OCR Plan 2.4): once, and only when something waits for it.
        let waiting_for_reader = machine.needs_reader > 0;
        self.per_workspace = per_workspace;
        self.machine = machine;
        if waiting_for_reader && self.model.is_none() && self.hub.is_some() {
            if self.built.is_none() && !self.worker_missing {
                self.learn_build();
            }
            self.publish();
            // The offer carries the download's real size: read the catalog
            // now, on the installer's thread, unless nothing could use it.
            if let Some(hub) = self.hub.as_ref() {
                hub.want_catalog(now);
            }
        } else {
            self.publish();
        }
    }

    /// Every request, oldest first.
    fn requests_in_order(&self) -> Vec<(WorkspaceId, String)> {
        self.mirror.all_requests().unwrap_or_default()
    }

    /// A request whose item the listing no longer holds — purged, or its
    /// workspace gone — can never be met: it goes. A trashed item keeps its
    /// request, and is read when it is restored.
    fn forget_requests_for_gone_items(&mut self, held: &HashSet<(WorkspaceId, String)>) {
        for (workspace, hash) in self.requests_in_order() {
            if !held.contains(&(workspace, hash.clone()))
                && let Err(error) = self.mirror.request_met(workspace, &hash)
            {
                self.stats.last_error = Some(error.0);
            }
        }
    }

    /// The request for this pair's bytes has been met.
    fn request_met(&mut self, pair: &Pair) {
        match self.mirror.request_met(pair.workspace, &pair.hash) {
            Ok(()) => self.stats.requests_met += 1,
            Err(error) => self.stats.last_error = Some(error.0),
        }
    }

    /// A window of a requested PDF is written: the item goes to the end of
    /// the requested lane, so whatever was added meanwhile — and any other
    /// request — goes before its next window.
    fn requeue(&mut self, pair: &Pair) {
        let at = self
            .plan
            .iter()
            .take_while(|p| p.fresh || p.requested)
            .count();
        let mut again = pair.clone();
        again.fresh = false;
        again.requested = true;
        self.plan.insert(at, again);
        self.counts.requested += 1;
    }

    /// Turn the two triggers into fresh pairs at the front.
    fn take_hints(&mut self, work: CoordinatorWork, now: i64) {
        let attempts = self.mirror.attempts(READER_TEXT).unwrap_or_default();
        let mut by_workspace: HashMap<WorkspaceId, Vec<DocumentId>> = HashMap::new();
        for (workspace, document) in work.documents {
            by_workspace.entry(workspace).or_default().push(document);
        }
        let mut fresh = Vec::new();
        let mut ignored = Tally::default();
        let mut recognize: Option<bool> = None;
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
                    &mut recognize,
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
                    &mut recognize,
                );
            }
        }
        // What was found but not queued (waiting for the reader, not here)
        // moved counts this pass cannot place without double counting: the
        // recompute that follows does, from scratch.
        if ignored.machine.unread > 0 {
            self.recount = true;
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
            if pair.requested {
                // **A request just made** (OCR Plan 3.2): behind every fresh
                // pair and every earlier request. A backlog pair of the same
                // item is lifted into the requested lane rather than queued
                // twice.
                if self
                    .waiting
                    .iter()
                    .any(|p| p.parent == pair.parent && p.hash == pair.hash)
                {
                    continue;
                }
                if let Some(i) = self
                    .plan
                    .iter()
                    .position(|p| p.parent == pair.parent && p.hash == pair.hash)
                {
                    if self.plan[i].requested || self.plan[i].fresh {
                        continue;
                    }
                    self.plan.remove(i);
                    self.counts.backlog = self.counts.backlog.saturating_sub(1);
                }
                let requested_at = self
                    .plan
                    .iter()
                    .take_while(|p| p.fresh || p.requested)
                    .count();
                self.plan.insert(requested_at, pair);
                self.counts.requested += 1;
                self.recount = true;
                continue;
            }
            if known.contains(&(pair.parent, pair.hash.clone())) {
                continue;
            }
            self.counts.fresh += 1;
            self.recount = true;
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
        tally: &mut Tally,
        recognize: &mut Option<bool>,
    ) {
        if references.is_empty() {
            return;
        }
        // One read of the workspace's rows: what is settled, and how.
        let outcomes = self.mirror.outcomes_in(workspace).unwrap_or_default();
        // What a person asked for here, and which entry carries each request:
        // the one already being read, else the first (OCR Plan 3.2).
        let requests: HashSet<String> = self
            .mirror
            .requests_in(workspace)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let carriers = carriers(&references, &requests, &outcomes);
        let mut seen: HashSet<(DocumentId, String)> = HashSet::new();
        let mut candidates: Vec<Candidate> = Vec::new();
        // Settled entries another entry holds a fuller reading of the same
        // bytes for: copied, never read (TECHNICAL §18, the review's note).
        let mut fuller_elsewhere = Vec::new();
        for reference in references {
            if reference.trashed {
                tally.queue.trashed += 1;
                continue;
            }
            // Two nodes of one page holding the same picture are one item.
            if !seen.insert((reference.document, reference.blob.clone())) {
                continue;
            }
            let hinted = reference.mime.as_deref().map(readable);
            if matches!(hinted, Some(None)) {
                continue;
            }
            let requested = carriers.get(&reference.blob) == Some(&reference.document);
            let outcome = outcomes.get(&(reference.document, reference.blob.clone()));
            let settled = outcome.and_then(super::mirror::Outcome::settled);
            if let Some(settled) = &settled {
                tally.settled(workspace, settled);
                if !requested {
                    if outcome.is_some_and(|held| {
                        self.fuller
                            .get(&reference.blob)
                            .is_some_and(|best| *best > held.rank())
                    }) {
                        fuller_elsewhere.push(reference);
                    }
                    continue;
                }
            }
            // A settled item a person asked for is already in the counts as
            // settled; nothing below counts it again.
            let counted_settled = settled.is_some();
            let mut needs_reader = matches!(hinted, Some(Some(Readable::Picture)))
                || (requested && outcome.is_some_and(needs_reader_to_go_on));
            let mut restart = false;
            if let Some(attempt) = attempts.get(&reference.blob) {
                match verdict(attempt, now) {
                    Verdict::Go => {}
                    // A text-only pass left pages for the reader: with the
                    // reader, it is read again from the start.
                    Verdict::NeedsReader => {
                        needs_reader = true;
                        restart = true;
                    }
                    Verdict::Backoff => {
                        tally.queue.backoff += 1;
                        if !counted_settled {
                            tally.unread(workspace);
                        }
                        continue;
                    }
                    Verdict::Failed => {
                        tally.queue.failed += 1;
                        if !counted_settled {
                            tally.failed_here(workspace, attempt.code.as_str());
                        }
                        // A request this computer cannot meet at this
                        // generation is not left standing.
                        if requested
                            && let Err(error) = self.mirror.request_met(workspace, &reference.blob)
                        {
                            self.stats.last_error = Some(error.0);
                        }
                        continue;
                    }
                }
            }
            if self
                .retry_after
                .get(&(reference.document, reference.blob.clone()))
                .is_some_and(|after| now < *after)
            {
                tally.queue.backoff += 1;
                if !counted_settled {
                    tally.unread(workspace);
                }
                continue;
            }
            candidates.push(Candidate {
                reference,
                needs_reader,
                restart,
                requested,
                counted_settled,
            });
        }
        if candidates.is_empty() && fuller_elsewhere.is_empty() {
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
        // **A copy follows the bytes' permission, not their presence**: the
        // Workspace's blob row is what lets it hold these words (TECHNICAL
        // §13); nothing is read, so the object need not be on this device.
        // Not counted: a settled entry is already in the counts as settled.
        for reference in fuller_elsewhere {
            let Some((size, row_mime)) = rows.get(&reference.blob) else {
                continue;
            };
            out.push(Pair {
                workspace,
                parent: reference.document,
                mime: reference.mime.clone().unwrap_or_else(|| row_mime.clone()),
                hash: reference.blob,
                size: *size,
                fresh,
                want: Want::Text,
                restart: false,
                counted: false,
                requested: false,
            });
        }
        for candidate in candidates {
            let Candidate {
                reference,
                needs_reader,
                restart,
                requested,
                counted_settled,
            } = candidate;
            // Counts for an item not already counted as settled.
            let mut scratch = Tally::default();
            let tally: &mut Tally = if counted_settled {
                &mut scratch
            } else {
                &mut *tally
            };
            let Some((size, row_mime)) = rows.get(&reference.blob) else {
                tally.not_here(workspace);
                continue;
            };
            let mime = reference.mime.clone().unwrap_or_else(|| row_mime.clone());
            let needs_reader = match readable(&mime) {
                Some(Readable::Text) => needs_reader,
                Some(Readable::Picture) => true,
                None => continue,
            };
            // **A picture needs the reader only where its bytes are**: one
            // whose bytes are elsewhere is not here, and no offer is made
            // for it (OCR Plan 2.4).
            if !self.objects.here(&reference.blob) {
                tally.not_here(workspace);
                continue;
            }
            // Recognition only when the worker said it recognizes. A PDF is
            // sent for recognition whenever it can be, so a scan is read in
            // one pass; a plain-text file never needs it.
            let want = if needs_reader || is_pdf(&mime) {
                let can = match *recognize {
                    Some(can) => can,
                    None => {
                        let can = self.recognizing();
                        *recognize = Some(can);
                        can
                    }
                };
                match (can, needs_reader) {
                    (true, _) => Want::Recognize,
                    (false, true) => {
                        tally.needs_reader(workspace);
                        continue;
                    }
                    (false, false) => Want::Text,
                }
            } else {
                Want::Text
            };
            tally.unread(workspace);
            out.push(Pair {
                workspace,
                parent: reference.document,
                hash: reference.blob,
                mime,
                size: *size,
                // A request has its own lane, between the two.
                fresh: fresh && !requested,
                want,
                restart: restart && want == Want::Recognize,
                counted: !fresh && !counted_settled,
                requested,
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
        let held = prior
            .as_ref()
            .filter(|reading| reading.is_settled_for_this_build());
        // The request still stands: the plan may be a moment old, and a
        // sibling's copy may have met it (OCR Plan 3.2).
        let requested = pair.requested
            && self
                .mirror
                .requested(pair.workspace, &pair.hash)
                .unwrap_or(false);

        // **Copy, don't read again**, when this store root already holds the
        // words for these bytes (TECHNICAL §9) — and give an entry that is
        // already settled the reading another entry holds when that one is
        // fuller (a capped `partial` read to the end, a higher generation),
        // by the same comparison the mirror picks a winner with (§18). A
        // request already under way reads on to its end: *Read again* means
        // reading, and its first window is what made it unsettled.
        if let Some(reading) = self.words_here(&state, pair)
            && held.is_none_or(|held| rank_of(&reading) > rank_of(held))
            && !(requested && held.is_none() && prior.is_some())
        {
            let refresh = held.is_some();
            self.set_stage(0, "copying", 1, 0);
            let outcome = self.copy(&state, pair, &reading);
            self.clear_stage();
            match outcome {
                Ok(true) => {
                    if refresh {
                        self.stats.refreshed += 1;
                    }
                    if requested {
                        self.request_met(pair);
                    }
                }
                Ok(false) => {}
                Err(why) => self.commit_failed(pair, now, why),
            }
            return Processed::Done;
        }
        if held.is_some() && !requested {
            self.stats.skipped += 1;
            return Processed::Done;
        }

        if let Availability::Unavailable(why) = self.launcher.availability() {
            return Processed::NoReader(why);
        }
        self.worker_down = None;
        // Queued for recognition, and the reader has gone since — removed, or
        // its model would not load: the recompute counts it as waiting.
        if pair.want == Want::Recognize && !self.recognizing() {
            self.stats.skipped += 1;
            return Processed::Done;
        }
        if let Err(why) = self.register(&state, pair) {
            self.commit_failed(pair, now, why);
            return Processed::Done;
        }
        let Ok(base) = fork(&state, pair.workspace, reading_id) else {
            self.commit_failed(pair, now, "the reading document did not fork".into());
            return Processed::Done;
        };
        drop(state);
        self.read(pair, requested, base, prior, now)
    }

    /// **The best words this store root holds for the pair's bytes**, in any
    /// workspace, under any other entry: what this coordinator finished since
    /// its last recompute (the mirror may not have applied it yet) and the
    /// mirror's best settled row, each read back from its document, the
    /// fuller by [`rank_of`]. `None` when no other entry holds a settled one.
    fn words_here(&self, state: &Arc<AppState>, pair: &Pair) -> Option<Reading> {
        let written = self
            .written
            .get(&pair.hash)
            .copied()
            .filter(|(_, parent)| *parent != pair.parent);
        let mirrored = self
            .mirror
            .settled_anywhere(&pair.hash, pair.parent)
            .ok()
            .flatten()
            .filter(|source| Some(*source) != written);
        [written, mirrored]
            .into_iter()
            .flatten()
            .filter_map(|(workspace, parent)| {
                let source = fork(state, workspace, reading_document_id(parent)).ok()?;
                ReadingDoc::from_doc(source).reading(&pair.hash)
            })
            .filter(Reading::is_settled_for_this_build)
            .max_by_key(rank_of)
    }

    /// Write a reading this store root already holds into this entry's
    /// reading document, in one change. Returns whether it was written.
    fn copy(
        &mut self,
        state: &Arc<AppState>,
        pair: &Pair,
        reading: &Reading,
    ) -> Result<bool, String> {
        self.register(state, pair)?;
        let base = fork(state, pair.workspace, reading_document_id(pair.parent))
            .map_err(|_| "the reading document did not fork".to_owned())?;
        let mut writer = Writer::new(pair.workspace, reading_document_id(pair.parent), base);
        match writer.doc.write(&pair.hash, reading) {
            Ok(Write::Applied) => {}
            Ok(Write::Superseded { .. }) => {
                self.stats.skipped += 1;
                return Ok(false);
            }
            Err(error) => return Err(error.to_string()),
        }
        writer.commit(state)?;
        self.stats.commits += 1;
        self.stats.copied += 1;
        self.settled_last = settled_of(reading.status, reading.reason);
        self.debug_line(0, pair, "copied", reading.pages.len());
        Ok(true)
    }

    /// Run the worker over one pair and write what it says.
    fn read(
        &mut self,
        pair: &Pair,
        requested: bool,
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
        // **A requested PDF is read in windows, with no cap overall** (OCR
        // Plan 3.2): a window that ends `partial/page-cap` is the window, not
        // the file.
        let windowed = requested && is_pdf(&pair.mime);
        // A `partial` with no reason at this generation is a reading that was
        // interrupted: resume after the last page it reached. **Unless this
        // is the reader reading what a text-only pass left** (OCR Plan 2.4):
        // that pass reached the last page, and the reader starts at the first.
        // A requested PDF also resumes a capped `partial` — that is *Read all
        // pages* — and starts a finished one again at page 1 (*Read again*).
        let prior = prior.filter(|_| !pair.restart);
        let resume = prior
            .as_ref()
            .filter(|r| {
                r.generation == GENERATION
                    && r.status == Some(ReadingStatus::Partial)
                    && (r.reason.is_none() || (windowed && r.reason == Some(Reason::PageCap)))
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
        // The reader's own token when the job may recognize — the installed
        // descriptor's signed engine, `pp-ocr` — and `text` otherwise.
        let reader = match pair.want {
            Want::Recognize => self
                .model
                .as_ref()
                .and_then(ModelInUse::reader)
                .unwrap_or_else(ReaderId::text),
            Want::Text => ReaderId::text(),
        };
        match writer.doc.begin(&pair.hash, &reader, GENERATION) {
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
            max_text_pages: Some(if windowed {
                REQUEST_TEXT_WINDOW
            } else {
                TEXT_PAGE_CAP
            }),
            max_read_pages: Some(if windowed {
                REQUEST_READ_WINDOW
            } else {
                koru_read::READ_PAGE_CAP
            }),
            want: pair.want,
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
        // Each recognized page's confidence: the reading's is their mean.
        let mut confidences: Vec<u8> = Vec::new();
        let outcome = self.launcher.read(&request, &mut |event| {
            if event.job() != Some(job) || terminal.is_some() {
                return;
            }
            match event {
                Event::Page {
                    page,
                    text,
                    source: from,
                    confidence,
                    ..
                } => {
                    if page < from_page {
                        return;
                    }
                    seen += 1;
                    if let Some(confidence) = confidence {
                        confidences.push(confidence.min(100));
                    }
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
            // **A window of a requested PDF is done** (OCR Plan 3.2): written
            // as progress — `partial`, no reason, how far it got — and the
            // item goes back in its lane, behind whatever arrived meanwhile.
            (
                Ok(()),
                Some(Event::Done {
                    status: ReadingStatus::Partial,
                    reason: Some(Reason::PageCap),
                    pages_done,
                    ..
                }),
            ) if windowed => {
                let pages_done = pages_done.max(last_page.unwrap_or(0));
                if pages_done < from_page {
                    // Nothing moved: never loop on a window that reads no
                    // page. A local failure, retried after its backoff.
                    self.record(&pair.hash, AttemptCode::Internal, now);
                    Ok("window-stalled")
                } else {
                    let confidence = mean(&confidences).or_else(|| {
                        prior
                            .as_ref()
                            .filter(|r| r.generation == GENERATION)
                            .and_then(|r| r.confidence)
                    });
                    let outcome = Outcome {
                        status: ReadingStatus::Partial,
                        reason: None,
                        source,
                        confidence,
                        pages_done: Some(pages_done),
                    };
                    writer
                        .put(&pair.hash, &mut buffer, Some(pages_done))
                        .and_then(|()| {
                            writer
                                .doc
                                .finish(&pair.hash, GENERATION, &outcome)
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        })
                        .and_then(|()| writer.commit(&state))
                        .map(|()| {
                            self.stats.commits += 1;
                            self.stats.windows += 1;
                            self.requeue(pair);
                            "window"
                        })
                }
            }
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
                // The mean over this job's recognized pages; on a resume
                // that recognized none, what the earlier pages said.
                let confidence = mean(&confidences).or_else(|| {
                    prior
                        .as_ref()
                        .filter(|r| r.generation == GENERATION)
                        .and_then(|r| r.confidence)
                });
                let finished = writer
                    .put(&pair.hash, &mut buffer, Some(pages_done))
                    .and_then(|()| {
                        let outcome = Outcome {
                            status,
                            reason,
                            source,
                            confidence,
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
                        if pair.want == Want::Recognize {
                            // Asked to recognize, and pages still need the
                            // reader: it is not working here.
                            self.recognition_lost();
                        }
                    } else if let Err(error) = self.mirror.clear_attempts(&pair.hash) {
                        self.stats.last_error = Some(error.0);
                    }
                    if requested && status.is_finished(reason) {
                        self.request_met(pair);
                    }
                    self.settled_last = settled_of(Some(status), reason);
                    status.as_str()
                })
            }
            (Ok(()), Some(Event::Failed { code, .. })) => {
                let kept = self.keep_progress(&state, &mut writer, pair, &mut buffer, last_page);
                // **The model would not load** (possibly after some pages):
                // recognition is off until the worker restarts.
                if code == FailureCode::NeedsReader && pair.want == Want::Recognize {
                    self.recognition_lost();
                }
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
    /// **Registered in batches** (TECHNICAL §9 "Registering during a
    /// backlog"): when the pair's reading document is not registered yet,
    /// every other pair the queue holds in the same workspace whose document
    /// is not registered either goes with it — fresh or backlog, in queue
    /// order, up to [`REGISTER_BATCH`] — in one manifest commit. A large
    /// import's files reach the fresh lane together and a restart's backlog
    /// is all queued at once, so either is a handful of manifest commits; a
    /// file added on its own is a batch of one, registered at once.
    ///
    /// Each pair's first write is still its own document commit, carrying
    /// its begin, pages and finish together: every reading document is its
    /// own document (D9), and the Store commits one document at a time.
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
        if !registered(&store, pair.parent) {
            let mut parents = vec![pair.parent];
            let mut taken: HashSet<DocumentId> = HashSet::from([pair.parent]);
            for queued in self.plan.iter().filter(|p| p.workspace == pair.workspace) {
                if parents.len() >= REGISTER_BATCH {
                    break;
                }
                if taken.insert(queued.parent) && !registered(&store, queued.parent) {
                    parents.push(queued.parent);
                }
            }
            let batch = store
                .ensure_readings(pair.workspace, &parents, Timestamp::now())
                .map_err(|e| format!("{:?}", e.code()))?;
            self.stats.registered += batch.created.len() as u64;
            if !batch.delta.is_empty() {
                self.stats.manifest_commits += 1;
                deltas.push(batch.delta);
            }
        }
        // The idempotent door, for the storage: an entry that arrived by sync
        // before its document's bytes is registered and has nothing to write
        // into yet.
        let one = store
            .ensure_reading(pair.workspace, pair.parent, Timestamp::now())
            .map_err(|e| format!("{:?}", e.code()))?;
        if one.created {
            self.stats.registered += 1;
            self.stats.manifest_commits += 1;
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

/// One reference the queue may take, and what it knows of it so far.
struct Candidate {
    reference: ReadableReference,
    needs_reader: bool,
    /// Read again from page 1: a text-only pass left pages for the reader.
    restart: bool,
    /// A person asked for it, and this entry carries the request.
    requested: bool,
    /// Already counted as settled by this pass.
    counted_settled: bool,
}

/// **Which entry carries each request** (OCR Plan 3.2): of the live entries
/// holding a requested hash, the one already being read (its reading is a
/// `partial` with no reason), else the first by id. One reading of the bytes
/// is made; the others receive it by the copy rule.
fn carriers(
    references: &[ReadableReference],
    requests: &HashSet<String>,
    outcomes: &HashMap<(DocumentId, String), super::mirror::Outcome>,
) -> HashMap<String, DocumentId> {
    let mut out: HashMap<String, (bool, DocumentId)> = HashMap::new();
    for reference in references {
        if reference.trashed || !requests.contains(&reference.blob) {
            continue;
        }
        let going = outcomes
            .get(&(reference.document, reference.blob.clone()))
            .is_some_and(|o| o.status.as_deref() == Some("partial") && o.reason.is_none());
        let candidate = (!going, reference.document);
        out.entry(reference.blob.clone())
            .and_modify(|held| {
                if (candidate.0, candidate.1.to_storage()) < (held.0, held.1.to_storage()) {
                    *held = candidate;
                }
            })
            .or_insert(candidate);
    }
    out.into_iter()
        .map(|(hash, (_, doc))| (hash, doc))
        .collect()
}

/// Whether going on with this reading needs the reader: some of what it holds
/// was recognized, so the pages after it may be scans too. A reading that
/// came from text layers alone does not — *Read all pages* on a PDF stopped at
/// the text-layer cap is finished by the worker alone (TECHNICAL §18).
fn needs_reader_to_go_on(outcome: &super::mirror::Outcome) -> bool {
    matches!(outcome.source.as_deref(), Some("recognized" | "mixed"))
}

/// What a finished reading counts as for a person, or `None` while it is not
/// finished.
fn settled_of(status: Option<ReadingStatus>, reason: Option<Reason>) -> Option<Settled> {
    let status = status?;
    if !status.is_finished(reason) {
        return None;
    }
    Some(match status {
        ReadingStatus::Partial => Settled::PartlyRead,
        ReadingStatus::Failed => {
            Settled::Failed(reason.map_or("damaged", Reason::as_str).to_owned())
        }
        ReadingStatus::Read | ReadingStatus::Empty => Settled::Read,
    })
}

/// The mean of recognized pages' confidences, rounded; `None` for none.
pub fn mean(confidences: &[u8]) -> Option<u8> {
    if confidences.is_empty() {
        return None;
    }
    let sum: u64 = confidences.iter().map(|c| u64::from(*c)).sum();
    let n = confidences.len() as u64;
    Some(((sum + n / 2) / n).min(100) as u8)
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
/// a fresh worker; an internal error (a picture or page past its time budget,
/// a file whose every page was lost) gets one retry after a backoff (OCR Plan
/// 2.4: a picture the worker gives up on is a local failure, retried once and
/// then left); an unreadable object is retried with backoff, a few times.
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
            let retries = if attempt.code == AttemptCode::Internal {
                2
            } else {
                MAX_RETRIES
            };
            if attempt.attempts >= retries {
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
