// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::providers::{
    ManagedProvider, ManagedProviderFactory, ManagedProviderRequest, ProviderDiagnostic,
    ProviderFailure, ProviderStage,
};
use koru_session_profiles::{CODEX_REGISTRATION, ManagedProviderRegistration};
use koru_session_protocol::{ModelLabel, ProviderModel, ProviderReadiness};
use std::sync::atomic::AtomicUsize;

/// A factory that lists `models` when asked, counting each listing, and —
/// when gated — holding the listing until the test opens the gate.
struct Fake {
    source: CatalogSource,
    calls: Arc<AtomicUsize>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    models: &'static [&'static str],
}

impl ManagedProviderFactory for Fake {
    fn create(&self, _: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        Err(ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Initialize,
            ProviderDiagnostic::HandshakeRejected,
        ))
    }

    fn discover_models(
        &self,
        _: std::ffi::OsString,
        _: PathBuf,
    ) -> Result<ProviderModels, ProviderFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (open, opened) = &*self.gate;
        let mut open = lock(open);
        while !*open {
            open = opened.wait(open).unwrap();
        }
        Ok(catalog(self.models))
    }

    fn catalog_source(&self) -> CatalogSource {
        self.source
    }
}

struct World {
    registry: Arc<ManagedProviderRegistry>,
    calls: Arc<AtomicUsize>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl World {
    /// `opencode` is listed in the background, `claude` captured by the
    /// probe, `codex` listed on demand.
    fn new(open: bool) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new((Mutex::new(open), Condvar::new()));
        let mut registry = ManagedProviderRegistry::default();
        for (kind, source, models) in [
            (
                "opencode",
                CatalogSource::Listing,
                &["opencode/beam-free"][..],
            ),
            ("claude", CatalogSource::Probe, &["default", "opus"][..]),
            ("codex", CatalogSource::OnDemand, &["gpt-5"][..]),
        ] {
            registry
                .register(
                    ManagedProviderRegistration {
                        kind,
                        ..CODEX_REGISTRATION
                    },
                    Arc::new(Fake {
                        source,
                        calls: calls.clone(),
                        gate: gate.clone(),
                        models,
                    }),
                )
                .unwrap();
        }
        Self {
            registry: Arc::new(registry),
            calls,
            gate,
        }
    }

    fn cache(&self, store: Arc<MemoryCatalogStore>, listings: bool) -> Arc<ProviderCatalogCache> {
        ProviderCatalogCache::new(
            self.registry.clone(),
            PathBuf::from("fixture-sessions"),
            Box::new(SharedStore(store)),
            listings,
        )
    }

    fn open(&self) {
        *lock(&self.gate.0) = true;
        self.gate.1.notify_all();
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The test keeps a handle on the store the cache owns.
struct SharedStore(Arc<MemoryCatalogStore>);

impl CatalogStore for SharedStore {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        self.0.load()
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        self.0.save(bytes)
    }
}

fn catalog(ids: &[&str]) -> ProviderModels {
    ids.iter()
        .map(|id| {
            let mut model = ProviderModel::new(id.parse().unwrap(), "fixture".parse().unwrap());
            model.name = ModelLabel::name(&format!("Name of {id}"));
            model
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn kind(name: &str) -> ProviderKind {
    name.parse().unwrap()
}

fn readiness(name: &str, availability: ExecutableAvailability) -> ProviderReadiness {
    ProviderReadiness {
        kind: kind(name),
        availability,
        capabilities: Default::default(),
        failure: None,
    }
}

fn available(name: &str, version: &str) -> ProviderReadiness {
    readiness(
        name,
        ExecutableAvailability::Available {
            version: Some(version.into()),
        },
    )
}

fn file(entries: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"format": CATALOG_FORMAT, "providers": entries}))
        .unwrap()
}

fn entry(koru: &str, version: &str, ids: &[&str]) -> serde_json::Value {
    serde_json::json!({"koru": koru, "version": version, "source": "listing",
        "models": serde_json::to_value(catalog(ids)).unwrap()})
}

fn ids(models: &ProviderModels) -> Vec<&str> {
    models
        .models()
        .iter()
        .map(|model| model.id.as_str())
        .collect()
}

#[test]
fn a_persisted_list_is_served_while_checking_and_fresh_again_on_the_same_version() {
    let world = World::new(true);
    let store = Arc::new(MemoryCatalogStore::with_bytes(file(serde_json::json!({
        "opencode": entry(KORU_VERSION, "1.19.5", &["opencode/old"]),
    }))));
    let cache = world.cache(store.clone(), false);
    assert_eq!(
        ids(&cache.serve(&kind("opencode")).unwrap()),
        ["opencode/old"],
        "dimmed during the first seconds"
    );
    cache.observe(&available("opencode", "1.19.5"), None);
    assert_eq!(
        ids(&cache.serve(&kind("opencode")).unwrap()),
        ["opencode/old"]
    );
    assert_eq!(world.calls(), 0, "the same version lists nothing again");
    assert_eq!(store.saves(), 0, "nothing changed, nothing written");
}

#[test]
fn a_new_agent_version_drops_its_entry_and_lists_it_again() {
    let world = World::new(true);
    let store = Arc::new(MemoryCatalogStore::with_bytes(file(serde_json::json!({
        "opencode": entry(KORU_VERSION, "1.18.0", &["opencode/old"]),
    }))));
    let cache = world.cache(store.clone(), true);
    cache.observe(&available("opencode", "1.19.5"), None);
    assert_eq!(
        ids(&cache.serve(&kind("opencode")).unwrap()),
        ["opencode/beam-free"],
        "the request waits on the background listing"
    );
    assert_eq!(world.calls(), 1);
    let written: serde_json::Value = serde_json::from_slice(&store.bytes().unwrap()).unwrap();
    assert_eq!(written["providers"]["opencode"]["version"], "1.19.5");
    assert_eq!(written["providers"]["opencode"]["source"], "listing");
}

#[test]
fn another_koru_version_and_an_invalid_entry_are_dropped_alone() {
    let world = World::new(true);
    let store = Arc::new(MemoryCatalogStore::with_bytes(file(serde_json::json!({
        "opencode": entry("0.0.0-another", "1.19.5", &["opencode/old"]),
        "claude": {"koru": KORU_VERSION, "version": null, "source": "probe",
                   "models": [{"id": "bad id", "group": "claude"}]},
        "codex": entry(KORU_VERSION, "0.153.4", &["gpt-5"]),
        "unregistered": entry(KORU_VERSION, "1", &["x"]),
    }))));
    let cache = world.cache(store, false);
    let state = cache.lock();
    assert_eq!(state.entries.keys().collect::<Vec<_>>(), [&kind("codex")]);
}

#[test]
fn the_probe_capture_is_fresh_and_written_once() {
    let world = World::new(true);
    let store = Arc::new(MemoryCatalogStore::default());
    let cache = world.cache(store.clone(), false);
    cache.observe(
        &available("claude", "0.76.0"),
        Some(catalog(&["default", "opus"])),
    );
    cache.observe(
        &available("claude", "0.76.0"),
        Some(catalog(&["default", "opus"])),
    );
    assert_eq!(
        store.saves(),
        1,
        "a capture that changed nothing is not written"
    );
    assert_eq!(
        ids(&cache.serve(&kind("claude")).unwrap()),
        ["default", "opus"]
    );
    assert_eq!(world.calls(), 0);

    // The file round-trips: a host started on it serves the same list.
    let reopened = world.cache(
        Arc::new(MemoryCatalogStore::with_bytes(store.bytes().unwrap())),
        false,
    );
    assert_eq!(
        ids(&reopened.serve(&kind("claude")).unwrap()),
        ["default", "opus"]
    );
}

#[test]
fn an_unknown_format_or_an_oversized_file_is_an_empty_cache() {
    let world = World::new(true);
    let entries = serde_json::json!({"codex": entry(KORU_VERSION, "1", &["gpt-5"])});
    let mut future = serde_json::from_slice::<serde_json::Value>(&file(entries.clone())).unwrap();
    future["format"] = serde_json::json!(CATALOG_FORMAT + 1);
    let mut oversized = file(entries);
    oversized.resize(MAX_CATALOG_FILE_BYTES + 1, b' ');
    for bytes in [
        serde_json::to_vec(&future).unwrap(),
        oversized,
        b"{not json".to_vec(),
    ] {
        let store = Arc::new(MemoryCatalogStore::with_bytes(bytes));
        let cache = world.cache(store, false);
        assert!(cache.lock().entries.is_empty());
    }
}

#[test]
fn an_agent_that_is_not_available_is_answered_at_once_without_a_listing() {
    let world = World::new(true);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), true);
    cache.observe(&readiness("codex", ExecutableAvailability::Missing), None);
    assert_eq!(
        cache.serve(&kind("codex")),
        Err(ErrorCode::ProviderUnavailable)
    );
    cache.observe(
        &readiness(
            "opencode",
            ExecutableAvailability::SignedOut {
                code: ErrorCode::ProviderIncompatible,
            },
        ),
        None,
    );
    assert_eq!(
        cache.serve(&kind("opencode")),
        Err(ErrorCode::ProviderIncompatible)
    );
    assert_eq!(
        cache.serve(&kind("unregistered")),
        Err(ErrorCode::CapabilityNotNegotiated)
    );
    assert_eq!(world.calls(), 0);
}

#[test]
fn an_on_demand_agent_is_listed_when_asked_and_then_served_from_memory() {
    let world = World::new(true);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), false);
    cache.observe(&available("codex", "0.153.4"), None);
    assert_eq!(ids(&cache.serve(&kind("codex")).unwrap()), ["gpt-5"]);
    assert_eq!(ids(&cache.serve(&kind("codex")).unwrap()), ["gpt-5"]);
    assert_eq!(world.calls(), 1);
}

/// The worker holds one of the two slots while it lists; a person's
/// request for the same agent waits on that listing rather than starting
/// its own, and the second slot stays free for anyone else.
#[test]
fn the_worker_holds_one_slot_and_a_request_during_its_listing_waits_for_it() {
    let world = World::new(false);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), true);
    cache.observe(&available("opencode", "1.19.5"), None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while world.calls() == 0 {
        assert!(Instant::now() < deadline, "the worker never listed");
        std::thread::yield_now();
    }
    let free = cache.slots().try_acquire();
    assert!(free.is_some(), "one slot is left for a person");
    assert!(
        cache.slots().try_acquire().is_none(),
        "the worker holds the other"
    );
    drop(free);
    let waiting = {
        let cache = cache.clone();
        std::thread::spawn(move || cache.serve(&kind("opencode")))
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(!waiting.is_finished(), "the request waits on the listing");
    world.open();
    assert_eq!(
        ids(&waiting.join().unwrap().unwrap()),
        ["opencode/beam-free"]
    );
    assert_eq!(world.calls(), 1, "one listing served both");
}
