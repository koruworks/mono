// SPDX-License-Identifier: MPL-2.0

//! **The host's model catalog**: each agent's model list, kept beside
//! readiness and answered from memory (agent-models TECHNICAL D2–D5).
//!
//! An entry comes from one of three places, which the agent's factory names
//! ([`CatalogSource`]): the readiness probe's own session start, which
//! already receives an ACP agent's list; a background listing on this
//! cache's one worker (OpenCode's catalog commands), run once per agent
//! version; or a listing made when somebody asks (Codex, which the picker
//! does not offer). Every listing waits for one of the host's two model
//! slots, and a person's runs on a thread of its own, so the request that
//! started it answers within its budget whether or not the list is ready.
//!
//! The file beside `profiles.conf` is a cache, never an authority. One that
//! is missing, unreadable, of another format or over 4 MiB is an empty
//! cache and is never quarantined; an entry written by another Koru version,
//! or that does not validate, is dropped alone. It is written only when an
//! entry changes, never on a timer, and nothing in it is synced.
//!
//! An agent whose session offers levels only for the model it is on
//! (Claude) has its models walked once per version on the same worker
//! (D16): the probe's capture is served meanwhile, with levels for its start
//! model alone, and the walk's levels are kept, and served, while the
//! version holds.
//!
//! An entry is keyed by the agent's kind, the version the readiness probe
//! read, and the Koru version. A persisted entry is served while its agent
//! still reads *Checking* — a dimmed list during the first seconds — and
//! is fresh again once the probe reads the same version.

use super::{CatalogSource, ManagedProviderRegistry};
use koru_session_protocol::{ErrorCode, ExecutableAvailability, ProviderKind, ProviderModels};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak, mpsc};
use std::time::{Duration, Instant};

/// The file's own format. Another number is an empty cache.
pub const CATALOG_FORMAT: u32 = 1;
/// A file larger than this is not read.
pub const MAX_CATALOG_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Listings the host runs at once: the worker's one, and one for a person.
const MODEL_SLOTS: usize = 2;
/// The whole time a request may take, waiting for a probe or listing included.
/// Every caller gives up at 30 s (the session client, the AI worker's reply,
/// the UI's HTTP client), so the host answers well inside that. A list not
/// ready by then is answered with `Backpressure` — still listing, ask again —
/// and the listing goes on and is kept.
const SERVE_BUDGET: Duration = Duration::from_secs(20);
/// When a failed background listing is tried again; after the last, it
/// waits for the next refresh or start.
const RETRY_AFTER: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
const KORU_VERSION: &str = env!("CARGO_PKG_VERSION");
/// An entry's `source`: the probe's capture, a listing of its own, or a
/// probe capture whose models have been walked for their levels (D16). Only
/// the last is read back: it says the walk is done for that version.
const SOURCE_PROBE: &str = "probe";
const SOURCE_LISTING: &str = "listing";
const SOURCE_WALKED: &str = "probe+walk";

/// Where the catalog's bytes live: a file beside `profiles.conf` in the
/// host, memory in a test. Replacement is whole and atomic; the store never
/// edits in place.
pub trait CatalogStore: Send + Sync {
    /// The stored bytes, `Ok(None)` when there are none, `Err` with a
    /// content-free reason when they cannot be read.
    fn load(&self) -> Result<Option<Vec<u8>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
}

/// A store that never touches disk.
#[derive(Default)]
pub struct MemoryCatalogStore {
    bytes: Mutex<Option<Vec<u8>>>,
    saves: std::sync::atomic::AtomicUsize,
}

impl MemoryCatalogStore {
    pub fn with_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Mutex::new(Some(bytes)),
            saves: Default::default(),
        }
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        lock(&self.bytes).clone()
    }

    pub fn saves(&self) -> usize {
        self.saves.load(Ordering::SeqCst)
    }
}

impl CatalogStore for MemoryCatalogStore {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.bytes())
    }

    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *lock(&self.bytes) = Some(bytes.to_vec());
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The host's listing slots. The worker lists one agent at a time, so it
/// holds at most one and a person's listing always has the other to wait
/// for.
pub struct ModelSlots {
    free: Mutex<usize>,
    released: Condvar,
}

pub struct ModelSlot(Arc<ModelSlots>);

impl ModelSlots {
    pub fn new(count: usize) -> Arc<Self> {
        Arc::new(Self {
            free: Mutex::new(count),
            released: Condvar::new(),
        })
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<ModelSlot> {
        let mut free = lock(&self.free);
        (*free > 0).then(|| {
            *free -= 1;
            ModelSlot(Arc::clone(self))
        })
    }

    pub fn acquire(self: &Arc<Self>) -> ModelSlot {
        let mut free = lock(&self.free);
        while *free == 0 {
            free = self
                .released
                .wait(free)
                .unwrap_or_else(|held| held.into_inner());
        }
        *free -= 1;
        ModelSlot(Arc::clone(self))
    }
}

impl Drop for ModelSlot {
    fn drop(&mut self) {
        *lock(&self.0.free) += 1;
        self.0.released.notify_one();
    }
}

/// One agent's entry as the file holds it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    koru: String,
    version: Option<String>,
    /// `probe`, `listing` or `probe+walk`; the last means the levels walk
    /// is done for this version.
    source: String,
    models: ProviderModels,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: u32,
    /// Each entry is read on its own, so one that does not validate is
    /// dropped without the others.
    providers: BTreeMap<String, serde_json::Value>,
}

struct Entry {
    stored: Stored,
    /// Captured or confirmed since this host started. A persisted entry
    /// that is not fresh is served only while its agent reads *Checking*.
    fresh: bool,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<ProviderKind, Entry>,
    readiness: BTreeMap<ProviderKind, ExecutableAvailability>,
    /// The readiness cache's sequence number of the last row seen per kind.
    /// An older row arriving late is ignored: rows are handed over after the
    /// readiness lock is released, so two can cross.
    seen: BTreeMap<ProviderKind, u64>,
    /// The listing under way per kind. One a drop has since made stale is
    /// not under way as far as anyone waiting is concerned ([`current_run`]).
    runs: BTreeMap<ProviderKind, Run>,
    /// How the last listing per kind ended, by run, for whoever waited on it.
    outcomes: BTreeMap<ProviderKind, (u64, Result<(), ErrorCode>)>,
    next_run: u64,
    /// Bumped whenever an entry is dropped, so a listing that started
    /// before the drop cannot put back what the drop removed.
    generations: BTreeMap<ProviderKind, u64>,
}

/// One listing: its own number, and the generation of the kind it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    id: u64,
    generation: u64,
}

/// Why an entry left the catalog, as the event says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogDrop {
    /// The probe read another version of the agent.
    Version,
    /// `RefreshProvider`: a CLI update, a manual refresh, a refused model.
    Refresh,
}

impl CatalogDrop {
    fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Refresh => "refresh",
        }
    }
}

pub struct ProviderCatalogCache {
    registry: Arc<ManagedProviderRegistry>,
    working_directory: PathBuf,
    store: Box<dyn CatalogStore>,
    slots: Arc<ModelSlots>,
    state: Mutex<State>,
    changed: Condvar,
    /// Held across a snapshot and its write, so writes land in order.
    saving: Mutex<()>,
    unwritable: AtomicBool,
    jobs: Option<Mutex<mpsc::Sender<ProviderKind>>>,
}

impl ProviderCatalogCache {
    /// Load the store and start the background worker. With `listings`
    /// off — the host's `no-provider-probes` test feature — nothing is ever
    /// listed: the catalog answers from what it was given.
    pub fn new(
        registry: Arc<ManagedProviderRegistry>,
        working_directory: PathBuf,
        store: Box<dyn CatalogStore>,
        listings: bool,
    ) -> Arc<Self> {
        let (send, receive) = mpsc::channel();
        let readiness = registry
            .registrations()
            .filter_map(|registration| registration.kind.parse().ok())
            .map(|kind| (kind, ExecutableAvailability::Checking))
            .collect();
        let cache = Arc::new(Self {
            registry,
            working_directory,
            store,
            slots: ModelSlots::new(MODEL_SLOTS),
            state: Mutex::new(State {
                readiness,
                ..State::default()
            }),
            changed: Condvar::new(),
            saving: Mutex::new(()),
            unwritable: AtomicBool::new(false),
            jobs: listings.then(|| Mutex::new(send)),
        });
        cache.load();
        if listings {
            let weak = Arc::downgrade(&cache);
            let spawned = std::thread::Builder::new()
                .name("koru-provider-catalog".into())
                .stack_size(256 * 1024)
                .spawn(move || run_worker(weak, receive));
            if spawned.is_err() {
                tracing::warn!(target: "koru::provider_models", event = "catalog.worker.unavailable");
            }
        }
        cache
    }

    fn load(&self) {
        let bytes = match self.store.load() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return,
            Err(reason) => return unreadable(&reason),
        };
        if bytes.len() > MAX_CATALOG_FILE_BYTES {
            return unreadable("oversized");
        }
        let file: File = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(_) => return unreadable("invalid"),
        };
        if file.format != CATALOG_FORMAT {
            return unreadable("format");
        }
        let mut state = self.lock();
        let mut dropped = 0_usize;
        for (kind, value) in file.providers {
            let entry = kind
                .parse::<ProviderKind>()
                .ok()
                .filter(|kind| state.readiness.contains_key(kind))
                .zip(serde_json::from_value::<Stored>(value).ok())
                .filter(|(_, stored)| stored.koru == KORU_VERSION);
            match entry {
                Some((kind, stored)) => {
                    state.entries.insert(
                        kind,
                        Entry {
                            stored,
                            fresh: false,
                        },
                    );
                }
                None => dropped += 1,
            }
        }
        tracing::info!(target: "koru::provider_models", event = "catalog.loaded",
            entries = state.entries.len(), dropped);
    }

    /// The one listing slot pool; a request answered elsewhere in the host
    /// shares it.
    pub fn slots(&self) -> &Arc<ModelSlots> {
        &self.slots
    }

    /// A readiness change, and what the probe's session start captured. The
    /// readiness cache calls this for every row it publishes, numbered by
    /// `sequence` in the order it published them; a row older than one
    /// already seen is ignored, capture and all. `drop` rides the row of a
    /// re-probe asked for because the list may be wrong, so the drop is
    /// ordered with the rows around it.
    pub fn observe(
        &self,
        readiness: &koru_session_protocol::ProviderReadiness,
        captured: Option<ProviderModels>,
        sequence: u64,
        drop: Option<CatalogDrop>,
    ) {
        let kind = readiness.kind.clone();
        let source = self.registry.catalog_source(&kind);
        let walks = self.registry.walks_model_levels(&kind);
        let mut save = false;
        let mut list = false;
        let mut walk = false;
        {
            let mut state = self.lock();
            if state.seen.get(&kind).is_some_and(|seen| *seen >= sequence) {
                return;
            }
            state.seen.insert(kind.clone(), sequence);
            if let Some(reason) = drop {
                save = state.entries.contains_key(&kind);
                drop_entry(&mut state, &kind, reason);
            }
            state
                .readiness
                .insert(kind.clone(), readiness.availability.clone());
            if let ExecutableAvailability::Available { version } = &readiness.availability {
                match (source, captured) {
                    (Some(CatalogSource::Probe), Some(models)) => {
                        // A walk already done for this version keeps its
                        // levels under the fresh capture.
                        let walked = state.entries.get(&kind).filter(|entry| {
                            walks
                                && version.is_some()
                                && entry.stored.version == *version
                                && entry.stored.source == SOURCE_WALKED
                        });
                        let (models, source) = match walked {
                            Some(entry) => (
                                keep_walked_levels(models, &entry.stored.models),
                                SOURCE_WALKED,
                            ),
                            None => (models, SOURCE_PROBE),
                        };
                        walk = walks && source == SOURCE_PROBE;
                        let stored = Stored {
                            koru: KORU_VERSION.into(),
                            version: version.clone(),
                            source: source.into(),
                            models,
                        };
                        save |= state
                            .entries
                            .get(&kind)
                            .is_none_or(|entry| entry.stored != stored);
                        captured_event(&kind, &stored);
                        state.entries.insert(
                            kind.clone(),
                            Entry {
                                stored,
                                fresh: true,
                            },
                        );
                    }
                    _ => match state.entries.get_mut(&kind) {
                        Some(entry) if entry.stored.version == *version => entry.fresh = true,
                        Some(_) => {
                            drop_entry(&mut state, &kind, CatalogDrop::Version);
                            save = true;
                        }
                        None => {}
                    },
                }
                list = source == Some(CatalogSource::Listing)
                    && !state.entries.contains_key(&kind)
                    && current_run(&state, &kind).is_none();
                walk &= current_run(&state, &kind).is_none();
            }
        }
        self.changed.notify_all();
        if save {
            self.persist();
        }
        if list || walk {
            self.queue(kind);
        }
    }

    /// Forget `kind`'s entry: a CLI update, a manual refresh or a refused
    /// model says it may be wrong. The re-probe that follows rebuilds it.
    pub fn drop_entry(&self, kind: &ProviderKind, reason: CatalogDrop) {
        let dropped = {
            let mut state = self.lock();
            let dropped = state.entries.contains_key(kind);
            drop_entry(&mut state, kind, reason);
            dropped
        };
        self.changed.notify_all();
        if dropped {
            self.persist();
        }
    }

    /// `DiscoverProviderModels`: from memory when the entry is there; after
    /// the probe or listing already under way; with a listing started on a
    /// thread of its own when the agent is ready and nothing is under way;
    /// and the readiness failure at once when the agent is not ready. All of
    /// it within [`SERVE_BUDGET`]: a list not ready by then is answered with
    /// `Backpressure`, and the listing goes on and is kept for the next ask.
    /// The asking thread never lists. The working directory is the host's:
    /// an agent's list is its user-level one.
    pub fn serve(self: &Arc<Self>, kind: &ProviderKind) -> Result<ProviderModels, ErrorCode> {
        self.serve_within(kind, SERVE_BUDGET)
    }

    fn serve_within(
        self: &Arc<Self>,
        kind: &ProviderKind,
        budget: Duration,
    ) -> Result<ProviderModels, ErrorCode> {
        let started = Instant::now();
        let deadline = started + budget;
        // The listing this request waits on, and whether it started it.
        let mut awaited: Option<u64> = None;
        let mut started_one = false;
        let mut state = self.lock();
        loop {
            let Some(availability) = state.readiness.get(kind).cloned() else {
                return Err(ErrorCode::CapabilityNotNegotiated);
            };
            let checking = availability == ExecutableAvailability::Checking;
            if let Some(entry) = state.entries.get(kind)
                && (entry.fresh || checking)
            {
                let served = if entry.fresh { "fresh" } else { "persisted" };
                tracing::debug!(target: "koru::provider_models", event = "catalog.served",
                    provider = %kind, state = served, elapsed_ms = started.elapsed().as_millis() as u64);
                return Ok(entry.stored.models.clone());
            }
            let run = current_run(&state, kind);
            match run {
                Some(run) => awaited = Some(run.id),
                None => {
                    if let Some(id) = awaited
                        && let Some((ended, Err(code))) = state.outcomes.get(kind)
                        && *ended == id
                    {
                        return Err(code.clone());
                    }
                }
            }
            match availability {
                ExecutableAvailability::Available { .. } if run.is_none() => {
                    // A listing this request started ended without a list it
                    // may serve — a drop made it stale. Ask again.
                    if started_one {
                        return Err(ErrorCode::Backpressure);
                    }
                    let run = claim(&mut state, kind);
                    if !self.spawn_listing(kind.clone(), run) {
                        state.runs.remove(kind);
                        return Err(ErrorCode::ProviderFailed);
                    }
                    awaited = Some(run.id);
                    started_one = true;
                }
                ExecutableAvailability::Available { .. } | ExecutableAvailability::Checking => {}
                ExecutableAvailability::Missing => return Err(ErrorCode::ProviderUnavailable),
                ExecutableAvailability::SignedOut { code }
                | ExecutableAvailability::Incompatible { code }
                | ExecutableAvailability::Failed { code } => return Err(code),
            }
            let Some(left) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                tracing::debug!(target: "koru::provider_models", event = "catalog.served",
                    provider = %kind, state = "listing", elapsed_ms = started.elapsed().as_millis() as u64);
                return Err(ErrorCode::Backpressure);
            };
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|held| held.into_inner())
                .0;
        }
    }

    /// A person's listing, on a thread of its own so the request can answer
    /// within its budget while the listing goes on. `false` when no thread
    /// could be started.
    fn spawn_listing(self: &Arc<Self>, kind: ProviderKind, run: Run) -> bool {
        let cache = Arc::clone(self);
        std::thread::Builder::new()
            .name("koru-provider-listing".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let _ = cache.list(&kind, run);
            })
            .is_ok()
    }

    /// Run one listing the caller has [`claim`]ed, holding one of the two
    /// slots for it, and keep what it found unless a drop has made it
    /// stale since. Whoever waited on it hears how it ended.
    fn list(&self, kind: &ProviderKind, run: Run) -> Result<(), ErrorCode> {
        let result = {
            let _slot = self.slots.acquire();
            let started = Instant::now();
            let result = self
                .registry
                .discover_models(kind, self.working_directory.clone())
                .map_err(|failure| failure.code);
            tracing::info!(target: "koru::provider_models", event = "catalog.listing.finished",
                provider = %kind, success = result.is_ok(),
                count = result.as_ref().map_or(0, |models| models.models().len()),
                elapsed_ms = started.elapsed().as_millis() as u64);
            result
        };
        let save = {
            let mut state = self.lock();
            if state.runs.get(kind) == Some(&run) {
                state.runs.remove(kind);
            }
            state.outcomes.insert(
                kind.clone(),
                (
                    run.id,
                    result.as_ref().map(|_| ()).map_err(|code| code.clone()),
                ),
            );
            let current = state.generations.get(kind).copied().unwrap_or_default();
            let version = match state.readiness.get(kind) {
                Some(ExecutableAvailability::Available { version }) => Some(version.clone()),
                _ => None,
            };
            match (&result, version) {
                (Ok(models), Some(version)) if current == run.generation => {
                    let stored = Stored {
                        koru: KORU_VERSION.into(),
                        version,
                        source: SOURCE_LISTING.into(),
                        models: models.clone(),
                    };
                    let save = state
                        .entries
                        .get(kind)
                        .is_none_or(|entry| entry.stored != stored);
                    captured_event(kind, &stored);
                    state.entries.insert(
                        kind.clone(),
                        Entry {
                            stored,
                            fresh: true,
                        },
                    );
                    save
                }
                _ => false,
            }
        };
        self.changed.notify_all();
        if save {
            self.persist();
        }
        result.map(|_| ())
    }

    /// Claim `kind` for the worker when it should be listed or walked now:
    /// a ready agent with nothing under way, and either background-listed
    /// with nothing fresh, or walked for its levels with a fresh capture the
    /// walk has not been through yet.
    fn claim_for_worker(&self, kind: &ProviderKind) -> Option<Run> {
        let mut state = self.lock();
        let ready = matches!(
            state.readiness.get(kind),
            Some(ExecutableAvailability::Available { .. })
        ) && current_run(&state, kind).is_none();
        let entry = state.entries.get(kind);
        let wanted = if self.registry.walks_model_levels(kind) {
            entry.is_some_and(|entry| entry.fresh && entry.stored.source == SOURCE_PROBE)
        } else {
            self.registry.catalog_source(kind) == Some(CatalogSource::Listing)
                && !entry.is_some_and(|entry| entry.fresh)
        };
        (ready && wanted).then(|| claim(&mut state, kind))
    }

    /// **The levels walk** (D16), on the worker, holding one slot: the
    /// agent's models, each with the levels it offers, laid over the fresh
    /// capture it was claimed for — unless a drop has made it stale since.
    /// A walk that fails keeps the capture as it is, and is tried again at
    /// the next refresh or start.
    fn walk(&self, kind: &ProviderKind, run: Run) {
        let started = Instant::now();
        let result = {
            let _slot = self.slots.acquire();
            self.registry
                .walk_model_levels(kind, self.working_directory.clone())
        };
        let save = {
            let mut state = self.lock();
            if state.runs.get(kind) == Some(&run) {
                state.runs.remove(kind);
            }
            state.outcomes.insert(kind.clone(), (run.id, Ok(())));
            let current = state.generations.get(kind).copied().unwrap_or_default();
            match (&result, state.entries.get_mut(kind)) {
                (Ok(walked), Some(entry))
                    if current == run.generation && entry.stored.source == SOURCE_PROBE =>
                {
                    entry.stored.models = walked_levels(&entry.stored.models, walked);
                    entry.stored.source = SOURCE_WALKED.into();
                    true
                }
                _ => false,
            }
        };
        let walked = result
            .as_ref()
            .map(|models| models.models())
            .unwrap_or_default();
        tracing::info!(target: "koru::provider_models", event = "catalog.walk.finished",
            provider = %kind, success = result.is_ok(), models = walked.len(),
            with_levels = walked.iter().filter(|model| model.reasoning.is_some()).count(),
            elapsed_ms = started.elapsed().as_millis() as u64);
        self.changed.notify_all();
        if save {
            self.persist();
        }
    }

    fn queue(&self, kind: ProviderKind) {
        if let Some(jobs) = &self.jobs {
            let _ = lock(jobs).send(kind);
        }
    }

    /// Write every entry, persisted and fresh alike. A write that fails
    /// keeps memory as it is, warns once, and is tried again at the next
    /// change.
    fn persist(&self) {
        let _saving = lock(&self.saving);
        let bytes = {
            let state = self.lock();
            let providers = state
                .entries
                .iter()
                .filter_map(|(kind, entry)| {
                    Some((
                        kind.as_str().to_owned(),
                        serde_json::to_value(&entry.stored).ok()?,
                    ))
                })
                .collect();
            let mut bytes = serde_json::to_vec_pretty(&File {
                format: CATALOG_FORMAT,
                providers,
            })
            .unwrap_or_default();
            bytes.push(b'\n');
            bytes
        };
        match self.store.save(&bytes) {
            Ok(()) => self.unwritable.store(false, Ordering::Relaxed),
            Err(reason) => {
                if !self.unwritable.swap(true, Ordering::Relaxed) {
                    tracing::warn!(target: "koru::provider_models", event = "catalog.unwritable", reason = %reason);
                }
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

/// Mark `kind` as being listed, under the same lock that saw it was not, as
/// a run of the kind's current generation.
fn claim(state: &mut State, kind: &ProviderKind) -> Run {
    state.next_run += 1;
    let run = Run {
        id: state.next_run,
        generation: state.generations.get(kind).copied().unwrap_or_default(),
    };
    state.runs.insert(kind.clone(), run);
    run
}

/// The listing under way for `kind`, unless a drop has made it stale: its
/// list would be thrown away, so nobody waits on it.
fn current_run(state: &State, kind: &ProviderKind) -> Option<Run> {
    let generation = state.generations.get(kind).copied().unwrap_or_default();
    state
        .runs
        .get(kind)
        .copied()
        .filter(|run| run.generation == generation)
}

fn drop_entry(state: &mut State, kind: &ProviderKind, reason: CatalogDrop) {
    *state.generations.entry(kind.clone()).or_default() += 1;
    if state.entries.remove(kind).is_some() {
        tracing::info!(target: "koru::provider_models", event = "catalog.dropped",
            provider = %kind, reason = reason.as_str());
    }
}

/// A fresh probe capture with the walk's levels kept: the capture's own
/// list, and for each model the capture has no levels for, those the walk
/// read. The capture's start model is the fresher fact and keeps its own.
fn keep_walked_levels(captured: ProviderModels, walked: &ProviderModels) -> ProviderModels {
    let models: Vec<_> = captured
        .models()
        .iter()
        .map(|model| {
            let mut model = model.clone();
            if model.reasoning.is_none() {
                model.reasoning = walked
                    .models()
                    .iter()
                    .find(|walked| walked.id == model.id)
                    .and_then(|walked| walked.reasoning.clone());
            }
            model
        })
        .collect();
    ProviderModels::try_from(models).unwrap_or(captured)
}

/// The capture's list with each model's levels as the walk read them; a
/// model the walk did not reach keeps what it had.
fn walked_levels(captured: &ProviderModels, walked: &ProviderModels) -> ProviderModels {
    let models: Vec<_> = captured
        .models()
        .iter()
        .map(|model| {
            let mut model = model.clone();
            if let Some(walked) = walked.models().iter().find(|walked| walked.id == model.id) {
                model.reasoning = walked.reasoning.clone();
            }
            model
        })
        .collect();
    ProviderModels::try_from(models).unwrap_or_else(|_| captured.clone())
}

fn captured_event(kind: &ProviderKind, stored: &Stored) {
    let models = stored.models.models();
    tracing::info!(target: "koru::provider_models", event = "catalog.captured",
        provider = %kind, count = models.len(),
        with_levels = models.iter().filter(|model| model.reasoning.is_some()).count(),
        source = %stored.source);
}

fn unreadable(reason: &str) {
    tracing::warn!(target: "koru::provider_models", event = "catalog.unreadable", reason);
}

/// The background worker: one listing at a time, each holding a slot. A
/// failed listing is tried again after 30 s, 2 min and 10 min; after that
/// it waits for the next refresh or start.
fn run_worker(cache: Weak<ProviderCatalogCache>, jobs: mpsc::Receiver<ProviderKind>) {
    let mut retries: BTreeMap<ProviderKind, (Instant, usize)> = BTreeMap::new();
    loop {
        let due = retries
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(kind, (at, _))| (kind.clone(), *at));
        let kind = match due {
            Some((kind, at)) => {
                match jobs.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(kind) => kind,
                    Err(mpsc::RecvTimeoutError::Timeout) => kind,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match jobs.recv() {
                Ok(kind) => kind,
                Err(_) => return,
            },
        };
        let Some(cache) = cache.upgrade() else {
            return;
        };
        let Some(run) = cache.claim_for_worker(&kind) else {
            retries.remove(&kind);
            continue;
        };
        if cache.registry.walks_model_levels(&kind) {
            cache.walk(&kind, run);
            continue;
        }
        match cache.list(&kind, run) {
            Ok(_) => {
                retries.remove(&kind);
            }
            Err(_) => {
                let failures = retries.get(&kind).map_or(0, |(_, failures)| *failures) + 1;
                match RETRY_AFTER.get(failures - 1) {
                    Some(after) => {
                        retries.insert(kind, (Instant::now() + *after, failures));
                    }
                    None => {
                        retries.remove(&kind);
                    }
                }
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|held| held.into_inner())
}

#[cfg(test)]
mod tests;
