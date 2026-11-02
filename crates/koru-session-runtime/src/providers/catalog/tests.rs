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

/// Hand the catalog a row the way the readiness cache does, numbered after
/// every row before it.
fn observe(
    cache: &ProviderCatalogCache,
    readiness: &ProviderReadiness,
    captured: Option<ProviderModels>,
) {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::SeqCst);
    cache.observe(readiness, captured, sequence, None);
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
    observe(&cache, &available("opencode", "1.19.5"), None);
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
    observe(&cache, &available("opencode", "1.19.5"), None);
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
    observe(
        &cache,
        &available("claude", "0.76.0"),
        Some(catalog(&["default", "opus"])),
    );
    observe(
        &cache,
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
    observe(
        &cache,
        &readiness("codex", ExecutableAvailability::Missing),
        None,
    );
    assert_eq!(
        cache.serve(&kind("codex")),
        Err(ErrorCode::ProviderUnavailable)
    );
    observe(
        &cache,
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
    observe(&cache, &available("codex", "0.153.4"), None);
    assert_eq!(ids(&cache.serve(&kind("codex")).unwrap()), ["gpt-5"]);
    assert_eq!(ids(&cache.serve(&kind("codex")).unwrap()), ["gpt-5"]);
    assert_eq!(world.calls(), 1);
}

/// A refresh drops the entry and writes the file without it, and a listing
/// that started before the drop cannot put the old list back.
#[test]
fn a_refresh_drops_the_entry_and_a_listing_from_before_cannot_restore_it() {
    let world = World::new(false);
    let store = Arc::new(MemoryCatalogStore::default());
    let cache = world.cache(store.clone(), false);
    observe(&cache, &available("codex", "1"), None);
    let listing = {
        let cache = cache.clone();
        std::thread::spawn(move || cache.serve(&kind("codex")))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while world.calls() == 0 {
        assert!(Instant::now() < deadline, "the listing never started");
        std::thread::yield_now();
    }
    cache.drop_entry(&kind("codex"), CatalogDrop::Refresh);
    assert_eq!(
        listing.join().unwrap(),
        Err(ErrorCode::Backpressure),
        "nobody waits on a listing a drop has made stale: ask again"
    );
    world.open();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache.lock().runs.is_empty() {
        assert!(Instant::now() < deadline, "the stale listing never ended");
        std::thread::yield_now();
    }
    assert!(
        !cache.lock().entries.contains_key(&kind("codex")),
        "and the catalog does not keep its list from before the refresh"
    );

    observe(
        &cache,
        &available("claude", "0.76.0"),
        Some(catalog(&["opus"])),
    );
    let saves = store.saves();
    cache.drop_entry(&kind("claude"), CatalogDrop::Refresh);
    assert_eq!(store.saves(), saves + 1);
    let written: serde_json::Value = serde_json::from_slice(&store.bytes().unwrap()).unwrap();
    assert!(written["providers"].get("claude").is_none());
    cache.drop_entry(&kind("claude"), CatalogDrop::Refresh);
    assert_eq!(store.saves(), saves + 1, "dropping nothing writes nothing");
}

/// The worker holds one of the two slots while it lists; a person's
/// request for the same agent waits on that listing rather than starting
/// its own, and the second slot stays free for anyone else.
#[test]
fn the_worker_holds_one_slot_and_a_request_during_its_listing_waits_for_it() {
    let world = World::new(false);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), true);
    observe(&cache, &available("opencode", "1.19.5"), None);
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

/// Finding 1: a request answers within its budget whether or not the list
/// is ready — `Backpressure`, ask again — and the listing it started goes
/// on and is kept, so the next ask is answered from memory.
#[test]
fn a_request_answers_within_its_budget_and_its_listing_is_kept() {
    let world = World::new(false);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), false);
    observe(&cache, &available("codex", "1"), None);
    let started = Instant::now();
    assert_eq!(
        cache.serve_within(&kind("codex"), Duration::from_millis(100)),
        Err(ErrorCode::Backpressure)
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "answered in time"
    );
    world.open();
    assert_eq!(
        ids(&cache
            .serve_within(&kind("codex"), Duration::from_secs(10))
            .unwrap()),
        ["gpt-5"]
    );
    assert_eq!(
        world.calls(),
        1,
        "the second ask waited on the first listing"
    );
}

/// Finding 3a: a probe's capture handed over late — after the re-probe that
/// dropped the list — cannot put the old list back; nor can one handed over
/// just before the drop survive it.
#[test]
fn a_capture_from_before_a_refresh_cannot_come_back() {
    let world = World::new(false);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), false);
    let checking = readiness("claude", ExecutableAvailability::Checking);
    cache.observe(
        &available("claude", "0.76.0"),
        Some(catalog(&["old"])),
        10,
        None,
    );
    cache.observe(&checking, None, 12, Some(CatalogDrop::Refresh));
    cache.observe(
        &available("claude", "0.76.0"),
        Some(catalog(&["late"])),
        11,
        None,
    );
    assert!(!cache.lock().entries.contains_key(&kind("claude")));
    assert_eq!(
        cache.lock().readiness[&kind("claude")],
        ExecutableAvailability::Checking,
        "the late row changed nothing"
    );

    cache.observe(
        &available("claude", "0.76.0"),
        Some(catalog(&["early"])),
        20,
        None,
    );
    cache.observe(&checking, None, 21, Some(CatalogDrop::Refresh));
    assert!(!cache.lock().entries.contains_key(&kind("claude")));
}

/// Finding 3b: a *Checking* row handed over after the *Available* row that
/// followed it does not leave the agent looking unchecked, so an ask is
/// listed at once instead of waiting out its budget.
#[test]
fn a_late_checking_row_does_not_hide_a_ready_agent() {
    let world = World::new(true);
    let cache = world.cache(Arc::new(MemoryCatalogStore::default()), false);
    cache.observe(&available("codex", "1"), None, 5, None);
    cache.observe(
        &readiness("codex", ExecutableAvailability::Checking),
        None,
        4,
        None,
    );
    let started = Instant::now();
    assert_eq!(
        ids(&cache
            .serve_within(&kind("codex"), Duration::from_secs(10))
            .unwrap()),
        ["gpt-5"]
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A probe-captured agent whose levels are walked (Claude's shape): its
/// capture has levels for the start model only, and the walk for every
/// model; each walk is counted.
struct Walker {
    walks: Arc<AtomicUsize>,
}

fn leveled(ids: &[&str]) -> ProviderModels {
    catalog(ids)
        .models()
        .iter()
        .map(|model| {
            let mut model = model.clone();
            model.reasoning = Some(koru_session_protocol::ModelReasoning {
                levels: vec![koru_session_protocol::LaunchReasoning::High],
                default: None,
                off: false,
            });
            model
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

/// The capture: levels on `default`, the model the session starts on.
fn captured() -> ProviderModels {
    let mut models = catalog(&["default", "opus"]).models().to_vec();
    models[0].reasoning = leveled(&["default"]).models()[0].reasoning.clone();
    models.try_into().unwrap()
}

impl ManagedProviderFactory for Walker {
    fn create(&self, _: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        Err(ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Initialize,
            ProviderDiagnostic::HandshakeRejected,
        ))
    }
    fn catalog_source(&self) -> CatalogSource {
        CatalogSource::Probe
    }
    fn walks_model_levels(&self) -> bool {
        true
    }
    fn walk_model_levels(
        &self,
        _: std::ffi::OsString,
        _: PathBuf,
    ) -> Result<ProviderModels, ProviderFailure> {
        self.walks.fetch_add(1, Ordering::SeqCst);
        Ok(leveled(&["default", "opus"]))
    }
}

fn walker() -> (Arc<ManagedProviderRegistry>, Arc<AtomicUsize>) {
    let walks = Arc::new(AtomicUsize::new(0));
    let mut registry = ManagedProviderRegistry::default();
    registry
        .register(
            ManagedProviderRegistration {
                kind: "claude",
                ..CODEX_REGISTRATION
            },
            Arc::new(Walker {
                walks: walks.clone(),
            }),
        )
        .unwrap();
    (Arc::new(registry), walks)
}

fn walking_cache(
    registry: &Arc<ManagedProviderRegistry>,
    store: Arc<MemoryCatalogStore>,
) -> Arc<ProviderCatalogCache> {
    ProviderCatalogCache::new(
        registry.clone(),
        PathBuf::from("fixture-sessions"),
        Box::new(SharedStore(store)),
        true,
    )
}

fn opus_has_levels(cache: &Arc<ProviderCatalogCache>) -> bool {
    cache.serve(&kind("claude")).unwrap().models()[1]
        .reasoning
        .is_some()
}

fn await_walked(cache: &Arc<ProviderCatalogCache>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cache
        .lock()
        .entries
        .get(&kind("claude"))
        .is_some_and(|entry| entry.stored.source == SOURCE_WALKED)
    {
        assert!(Instant::now() < deadline, "the walk never landed");
        std::thread::yield_now();
    }
}

/// **D16: a walking agent is walked once per version.** Its capture is
/// served at once, with levels for the start model; the walk's levels land
/// after, are written, survive a fresh capture and a restart on the same
/// version, and a new version walks again.
#[test]
fn a_walking_agent_is_walked_once_per_version_and_keeps_its_levels() {
    let (registry, walks) = walker();
    let store = Arc::new(MemoryCatalogStore::default());
    let cache = walking_cache(&registry, store.clone());
    observe(&cache, &available("claude", "0.76.0"), Some(captured()));
    await_walked(&cache);
    assert_eq!(walks.load(Ordering::SeqCst), 1);
    assert!(opus_has_levels(&cache), "the walk's levels are served");

    observe(&cache, &available("claude", "0.76.0"), Some(captured()));
    assert!(opus_has_levels(&cache), "a fresh capture keeps them");
    let written: serde_json::Value = serde_json::from_slice(&store.bytes().unwrap()).unwrap();
    assert_eq!(written["providers"]["claude"]["source"], "probe+walk");

    let reopened = walking_cache(
        &registry,
        Arc::new(MemoryCatalogStore::with_bytes(store.bytes().unwrap())),
    );
    observe(&reopened, &available("claude", "0.76.0"), Some(captured()));
    assert!(opus_has_levels(&reopened), "a restart keeps them too");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(walks.load(Ordering::SeqCst), 1, "and walks nothing again");

    observe(&cache, &available("claude", "0.77.0"), Some(captured()));
    await_walked(&cache);
    assert_eq!(walks.load(Ordering::SeqCst), 2, "a new version is walked");
}

#[test]
fn walked_levels_lay_over_the_capture_and_a_model_not_reached_keeps_its_own() {
    let mut walked = leveled(&["default"]).models().to_vec();
    walked[0].reasoning = None;
    let walked: ProviderModels = walked.try_into().unwrap();
    let laid = walked_levels(&captured(), &walked);
    assert!(laid.models()[0].reasoning.is_none(), "the walk read none");
    assert!(laid.models()[1].reasoning.is_none(), "opus was not reached");
    let kept = keep_walked_levels(captured(), &leveled(&["default", "opus"]));
    assert!(kept.models().iter().all(|model| model.reasoning.is_some()));
}

fn modes(ids: &[&str], current: &str) -> AgentModes {
    AgentModes::new(
        ids.iter()
            .map(|id| koru_session_protocol::AgentMode::new(id.parse().unwrap()))
            .collect(),
        current.parse().ok(),
    )
    .unwrap()
}

/// **An agent's approval modes ride beside its list** (koru-ai D7): those a
/// ready probe read replace the last ones, a drop forgets them, and the file
/// never holds them.
#[test]
fn a_probes_modes_are_served_beside_the_list_and_never_written() {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let next = || SEQUENCE.fetch_add(1, Ordering::SeqCst);
    let world = World::new(true);
    let store = Arc::new(MemoryCatalogStore::default());
    let cache = world.cache(store.clone(), false);
    assert_eq!(cache.modes(&kind("claude")), None);

    let read = modes(&["default", "plan"], "default");
    cache.observe_capture(
        &available("claude", "0.76.0"),
        ProbeCapture {
            models: Some(catalog(&["default", "opus"])),
            modes: Some(read.clone()),
        },
        next(),
    );
    assert_eq!(cache.modes(&kind("claude")), Some(read));
    assert!(
        !String::from_utf8(store.bytes().unwrap())
            .unwrap()
            .contains("plan"),
        "the file keeps its format"
    );

    // A row that is not ready says nothing about modes.
    cache.observe_capture(
        &readiness("claude", ExecutableAvailability::Checking),
        ProbeCapture::default(),
        next(),
    );
    assert!(cache.modes(&kind("claude")).is_some());
    cache.drop_entry(&kind("claude"), CatalogDrop::Refresh);
    assert_eq!(cache.modes(&kind("claude")), None);
}

/// An agent that advertises none has the ones Koru gives it.
#[test]
fn an_agent_without_modes_of_its_own_is_served_its_factorys() {
    struct Fixed;
    impl ManagedProviderFactory for Fixed {
        fn create(&self, _: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
            Err(ProviderFailure::new(
                ErrorCode::ProviderFailed,
                ProviderStage::Initialize,
                ProviderDiagnostic::HandshakeRejected,
            ))
        }
        fn fixed_modes(&self) -> Option<AgentModes> {
            Some(modes(&["ask", "full-access"], "ask"))
        }
    }
    let mut registry = ManagedProviderRegistry::default();
    registry
        .register(CODEX_REGISTRATION, Arc::new(Fixed))
        .unwrap();
    let cache = ProviderCatalogCache::new(
        Arc::new(registry),
        PathBuf::from("fixture-sessions"),
        Box::new(MemoryCatalogStore::default()),
        false,
    );
    assert_eq!(
        cache.modes(&kind("codex")),
        Some(modes(&["ask", "full-access"], "ask"))
    );
}
