// SPDX-License-Identifier: MPL-2.0

//! **The host's model catalog**: each agent's model list, kept beside
//! readiness and answered from memory (agent-models TECHNICAL D2–D5).
//!
//! An entry comes from one of three places, which the agent's factory names
//! ([`CatalogSource`]): the readiness probe's own session start, which
//! already receives an ACP agent's list; a background listing on this
//! cache's one worker (OpenCode's catalog commands), run once per agent
//! version; or a listing made when somebody asks (Codex, which the picker
//! does not offer). Every listing holds one of the host's two model slots;
//! the worker waits for one, a person's request takes one or is refused.
//!
//! The file beside `profiles.conf` is a cache, never an authority. One that
//! is missing, unreadable, of another format or over 4 MiB is an empty
//! cache and is never quarantined; an entry written by another Koru version,
//! or that does not validate, is dropped alone. It is written only when an
//! entry changes, never on a timer, and nothing in it is synced.
//!
//! An entry is keyed by the agent's kind, the version the readiness probe
//! read, and the Koru version. A persisted entry is served while its agent
//! still reads *Checking* — a dimmed list during the first seconds — and
//! is fresh again once the probe reads the same version.

use super::{CatalogSource, ManagedProviderRegistry};
use koru_session_protocol::{ErrorCode, ExecutableAvailability, ProviderKind, ProviderModels};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
/// How long a request waits for a probe or a listing already under way: the
/// longest adapter budget (OpenCode's 25 s) and some.
const WAIT_BUDGET: Duration = Duration::from_secs(30);
/// When a failed background listing is tried again; after the last, it
/// waits for the next refresh or start.
const RETRY_AFTER: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
const KORU_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// The host's listing slots, shared by the worker (which waits for one)
/// and a person's request (which takes one or is refused with
/// `Backpressure`), so one slot is always left for a person.
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
    /// For diagnosis only: `probe` or `listing`.
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
    listing: BTreeSet<ProviderKind>,
    /// Bumped whenever an entry is dropped, so a listing that started
    /// before the drop cannot put back what the drop removed.
    generations: BTreeMap<ProviderKind, u64>,
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
    /// readiness cache calls this for every row it publishes.
    pub fn observe(
        &self,
        readiness: &koru_session_protocol::ProviderReadiness,
        captured: Option<ProviderModels>,
    ) {
        let kind = readiness.kind.clone();
        let source = self.registry.catalog_source(&kind);
        let mut save = false;
        let mut list = false;
        {
            let mut state = self.lock();
            state
                .readiness
                .insert(kind.clone(), readiness.availability.clone());
            if let ExecutableAvailability::Available { version } = &readiness.availability {
                match (source, captured) {
                    (Some(CatalogSource::Probe), Some(models)) => {
                        let stored = Stored {
                            koru: KORU_VERSION.into(),
                            version: version.clone(),
                            source: "probe".into(),
                            models,
                        };
                        save = state
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
                    && !state.listing.contains(&kind);
            }
        }
        self.changed.notify_all();
        if save {
            self.persist();
        }
        if list {
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
    /// the probe or listing already under way, within its budget; listed on
    /// demand when the agent is ready and nothing is; and the readiness
    /// failure at once when it is not. The working directory is the host's:
    /// an agent's list is its user-level one.
    pub fn serve(&self, kind: &ProviderKind) -> Result<ProviderModels, ErrorCode> {
        let started = Instant::now();
        let deadline = started + WAIT_BUDGET;
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
            let listing = state.listing.contains(kind);
            match availability {
                ExecutableAvailability::Available { .. } if !listing => {
                    let generation = claim(&mut state, kind);
                    drop(state);
                    return self.list(kind, generation, false);
                }
                ExecutableAvailability::Available { .. } | ExecutableAvailability::Checking => {}
                ExecutableAvailability::Missing => return Err(ErrorCode::ProviderUnavailable),
                ExecutableAvailability::SignedOut { code }
                | ExecutableAvailability::Incompatible { code }
                | ExecutableAvailability::Failed { code } => return Err(code),
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ErrorCode::ProviderUnavailable);
            };
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|held| held.into_inner())
                .0;
        }
    }

    /// Run one listing the caller has [`claim`]ed, and keep what it found.
    /// The worker waits for a slot; a person's request takes one or is
    /// refused.
    fn list(
        &self,
        kind: &ProviderKind,
        generation: u64,
        wait: bool,
    ) -> Result<ProviderModels, ErrorCode> {
        let slot = if wait {
            Some(self.slots.acquire())
        } else {
            self.slots.try_acquire()
        };
        let result = match slot {
            Some(_slot) => {
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
            }
            None => Err(ErrorCode::Backpressure),
        };
        let save = {
            let mut state = self.lock();
            state.listing.remove(kind);
            let current = state.generations.get(kind).copied().unwrap_or_default();
            let version = match state.readiness.get(kind) {
                Some(ExecutableAvailability::Available { version }) => Some(version.clone()),
                _ => None,
            };
            match (&result, version) {
                (Ok(models), Some(version)) if current == generation => {
                    let stored = Stored {
                        koru: KORU_VERSION.into(),
                        version,
                        source: "listing".into(),
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
        result
    }

    /// Claim `kind` for the worker when it should be listed now: a
    /// background-listed agent that is ready, with nothing fresh and nothing
    /// under way.
    fn claim_for_worker(&self, kind: &ProviderKind) -> Option<u64> {
        let mut state = self.lock();
        let wanted = self.registry.catalog_source(kind) == Some(CatalogSource::Listing)
            && matches!(
                state.readiness.get(kind),
                Some(ExecutableAvailability::Available { .. })
            )
            && !state.listing.contains(kind)
            && !state.entries.get(kind).is_some_and(|entry| entry.fresh);
        wanted.then(|| claim(&mut state, kind))
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

/// Mark `kind` as being listed, under the same lock that saw it was not,
/// and say which generation the listing belongs to.
fn claim(state: &mut State, kind: &ProviderKind) -> u64 {
    state.listing.insert(kind.clone());
    state.generations.get(kind).copied().unwrap_or_default()
}

fn drop_entry(state: &mut State, kind: &ProviderKind, reason: CatalogDrop) {
    *state.generations.entry(kind.clone()).or_default() += 1;
    if state.entries.remove(kind).is_some() {
        tracing::info!(target: "koru::provider_models", event = "catalog.dropped",
            provider = %kind, reason = reason.as_str());
    }
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
        let Some(generation) = cache.claim_for_worker(&kind) else {
            retries.remove(&kind);
            continue;
        };
        match cache.list(&kind, generation, true) {
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
