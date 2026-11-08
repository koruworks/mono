// SPDX-License-Identifier: MPL-2.0

//! Host-owned readiness. Only explicit refresh starts a probe; every reader
//! sees accepted generations of the same cache. Every row it publishes, and
//! the model list a probe's session start received, also go to the model
//! catalog beside it.
use super::{
    CatalogDrop, DiscoveryRun, ManagedProviderRegistry, ProbeCapture, ProbeLimits,
    ProviderCatalogCache, ProviderDiagnostic, ProviderDiscovery, ProviderFailure, ProviderStage,
};
use koru_session_protocol::{ErrorCode, ExecutableAvailability, ProviderKind, ProviderReadiness};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

const SUBSCRIBER_CAPACITY: usize = 32;

struct Entry {
    generation: u64,
    active: usize,
    pending: Option<u64>,
    readiness: ProviderReadiness,
}

struct State {
    entries: BTreeMap<ProviderKind, Entry>,
    subscribers: Vec<mpsc::SyncSender<ProviderReadiness>>,
    /// Numbers every published row, under this lock. The catalog is handed
    /// rows after the lock is released, so two can reach it out of order;
    /// it keeps the newest by this number.
    published: u64,
}

pub struct ProviderReadinessCache {
    registry: Arc<ManagedProviderRegistry>,
    working_directory: PathBuf,
    probes_enabled: bool,
    discovery_timeout: std::time::Duration,
    state: Mutex<State>,
    catalog: Option<Arc<ProviderCatalogCache>>,
}

impl ProviderReadinessCache {
    pub fn new(registry: Arc<ManagedProviderRegistry>, working_directory: PathBuf) -> Arc<Self> {
        Self::with_probe_policy(registry, working_directory, true)
    }

    /// The host's no-provider-probes test feature disables every probe entry,
    /// including explicit refresh, while keeping the real cache and wire path.
    pub fn with_probe_policy(
        registry: Arc<ManagedProviderRegistry>,
        working_directory: PathBuf,
        probes_enabled: bool,
    ) -> Arc<Self> {
        Self::build(registry, working_directory, probes_enabled, None)
    }

    /// The host's cache: as [`Self::with_probe_policy`], and every row it
    /// publishes reaches `catalog` with what the probe captured.
    pub fn with_catalog(
        registry: Arc<ManagedProviderRegistry>,
        working_directory: PathBuf,
        probes_enabled: bool,
        catalog: Arc<ProviderCatalogCache>,
    ) -> Arc<Self> {
        Self::build(registry, working_directory, probes_enabled, Some(catalog))
    }

    fn build(
        registry: Arc<ManagedProviderRegistry>,
        working_directory: PathBuf,
        probes_enabled: bool,
        catalog: Option<Arc<ProviderCatalogCache>>,
    ) -> Arc<Self> {
        let entries = registry
            .registrations()
            .map(|registration| {
                let kind: ProviderKind = registration.kind.parse().expect("validated registration");
                (
                    kind.clone(),
                    Entry {
                        generation: 0,
                        active: 0,
                        pending: None,
                        readiness: checking(kind),
                    },
                )
            })
            .collect();
        Arc::new(Self {
            registry,
            working_directory,
            probes_enabled,
            discovery_timeout: ProbeLimits::default().timeout.saturating_mul(4),
            state: Mutex::new(State {
                entries,
                subscribers: Vec::new(),
                published: 0,
            }),
            catalog,
        })
    }

    /// Snapshot and stream registration happen under the same lock, so no
    /// accepted change can be missed between reading and subscribing.
    pub fn subscribe(&self) -> (Vec<ProviderReadiness>, mpsc::Receiver<ProviderReadiness>) {
        let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
        let snapshot = state
            .entries
            .values()
            .map(|entry| entry.readiness.clone())
            .collect();
        let (send, receive) = mpsc::sync_channel(SUBSCRIBER_CAPACITY);
        state.subscribers.push(send);
        (snapshot, receive)
    }

    pub fn snapshot(&self) -> Vec<ProviderReadiness> {
        self.state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .entries
            .values()
            .map(|entry| entry.readiness.clone())
            .collect()
    }

    pub fn refresh_all(self: &Arc<Self>) {
        for provider in self.snapshot() {
            let _ = self.refresh(provider.kind);
        }
    }

    /// The answering thread only advances a generation and starts a worker.
    /// Construction, executable lookup and the complete handshake are off it.
    pub fn refresh(self: &Arc<Self>, kind: ProviderKind) -> Result<(), ErrorCode> {
        self.refresh_with(kind, None)
    }

    /// `RefreshProvider`: a re-probe asked for because something about the
    /// agent changed, so its model list is dropped with it. The drop rides
    /// the re-probe's own *Checking* row, so a capture from the probe before
    /// it, arriving late, cannot put the old list back.
    pub fn refresh_and_rebuild_models(
        self: &Arc<Self>,
        kind: ProviderKind,
    ) -> Result<(), ErrorCode> {
        self.refresh_with(kind, Some(CatalogDrop::Refresh))
    }

    fn refresh_with(
        self: &Arc<Self>,
        kind: ProviderKind,
        drop: Option<CatalogDrop>,
    ) -> Result<(), ErrorCode> {
        let (generation, start, changed, sequence) = {
            let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
            let entry = state
                .entries
                .get_mut(&kind)
                .ok_or(ErrorCode::ProviderIncompatible)?;
            entry.generation = entry
                .generation
                .checked_add(1)
                .ok_or(ErrorCode::ProviderFailed)?;
            entry.readiness = checking(kind.clone());
            let generation = entry.generation;
            // At most two probes per kind may overlap. Further retries
            // coalesce to the latest generation without unbounded processes.
            let start = self.probes_enabled && entry.active < 2;
            if start {
                entry.active += 1;
            } else if self.probes_enabled {
                entry.pending = Some(generation);
                // Both actual workers still own their permits. The newest
                // queued retry must remain finite even if they never return.
                entry.readiness = failed(kind.clone(), ProviderDiagnostic::Backpressure);
            }
            let changed = entry.readiness.clone();
            let sequence = publish(&mut state, changed.clone());
            (generation, start, changed, sequence)
        };
        if let Some(catalog) = &self.catalog {
            catalog.observe(&changed, None, sequence, drop);
        }
        if !self.probes_enabled {
            self.complete(
                generation,
                failed(kind, ProviderDiagnostic::InvalidState),
                ProbeCapture::default(),
            );
        } else if start {
            self.spawn_probe(kind, generation);
        }
        Ok(())
    }

    fn spawn_probe(self: &Arc<Self>, kind: ProviderKind, generation: u64) {
        let weak = Arc::downgrade(self);
        let registry = Arc::clone(&self.registry);
        let cwd = self.working_directory.clone();
        let requested = kind.clone();
        let timeout = self.discovery_timeout;
        let spawned = std::thread::Builder::new()
            .name("koru-provider-readiness".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let (result, capture, run) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        discover(&registry, &requested, cwd, timeout)
                    }))
                    .unwrap_or_else(|_| {
                        (
                            failed(requested.clone(), ProviderDiagnostic::UnexpectedExit),
                            ProbeCapture::default(),
                            None,
                        )
                    });
                if let Some(cache) = weak.upgrade() {
                    cache.complete(generation, result, capture);
                }
                // A visible timeout does not mean the provider stopped.
                // Hold this permit until its actual worker exits.
                if let Some(run) = run {
                    run.wait_until_finished();
                }
                if let Some(cache) = weak.upgrade() {
                    cache.finished(requested);
                }
            });
        if spawned.is_err() {
            self.complete(
                generation,
                failed(kind.clone(), ProviderDiagnostic::Spawn),
                ProbeCapture::default(),
            );
            self.finished(kind);
        }
    }

    fn finished(self: &Arc<Self>, kind: ProviderKind) {
        let pending = {
            let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
            let entry = state.entries.get_mut(&kind).expect("registered probe");
            entry.active -= 1;
            let pending = entry.pending.take();
            if pending.is_some() {
                entry.active += 1;
            }
            pending
        };
        if let Some(generation) = pending {
            self.spawn_probe(kind, generation);
        }
    }

    /// Accept a probe's answer if it is still the latest asked for, and
    /// hand the catalog the row and what the probe's start captured. A
    /// stale answer reaches neither.
    fn complete(&self, generation: u64, result: ProviderReadiness, capture: ProbeCapture) -> bool {
        let sequence = {
            let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
            let Some(entry) = state.entries.get_mut(&result.kind) else {
                return false;
            };
            if entry.generation != generation {
                return false;
            }
            entry.readiness = result.clone();
            publish(&mut state, result.clone())
        };
        if let Some(catalog) = &self.catalog {
            catalog.observe_capture(&result, capture, sequence);
        }
        true
    }
}

fn checking(kind: ProviderKind) -> ProviderReadiness {
    ProviderReadiness {
        kind,
        availability: ExecutableAvailability::Checking,
        capabilities: Default::default(),
        failure: None,
    }
}

fn failed(kind: ProviderKind, diagnostic: ProviderDiagnostic) -> ProviderReadiness {
    super::discovery::classify_failure(ProviderFailure::new(
        ErrorCode::ProviderFailed,
        ProviderStage::Discovery,
        diagnostic,
    ))
    .readiness(kind)
}

fn discover(
    registry: &ManagedProviderRegistry,
    kind: &ProviderKind,
    cwd: PathBuf,
    timeout: std::time::Duration,
) -> (ProviderReadiness, ProbeCapture, Option<DiscoveryRun>) {
    let limits = ProbeLimits::default();
    let mut owner = None;
    let result = registry.compatibility_probe(kind, cwd).and_then(|probe| {
        if probe.provider() != *kind {
            return Err(ProviderFailure::new(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Discovery,
                ProviderDiagnostic::HandshakeRejected,
            ));
        }
        let run = DiscoveryRun::spawn(vec![probe], limits)?;
        // Every adapter already bounds its probe. This outer bound also makes a
        // broken probe an honest failure instead of an everlasting Checking.
        let answer = run.recv_timeout(timeout);
        owner = Some(run);
        answer?.ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ProviderFailed,
                ProviderStage::Discovery,
                ProviderDiagnostic::Timeout,
            )
        })
    });
    let mut capture = ProbeCapture::default();
    let readiness = match result {
        Ok(ProviderDiscovery {
            managed,
            models,
            modes,
            ..
        }) => {
            capture = ProbeCapture { models, modes };
            managed.readiness(kind.clone())
        }
        Err(failure) => super::discovery::classify_failure(failure).readiness(kind.clone()),
    };
    (readiness, capture, owner)
}

/// Send a row to every subscriber and number it; the number orders the
/// row's hand-over to the catalog.
fn publish(state: &mut State, result: ProviderReadiness) -> u64 {
    state.published += 1;
    let sequence = state.published;
    // A slow reader loses its subscription; it cannot hold the host or retain
    // unbounded results. Reconnection obtains a new atomic snapshot.
    state
        .subscribers
        .retain(|send| send.try_send(result.clone()).is_ok());
    sequence
}

#[cfg(test)]
mod tests;
