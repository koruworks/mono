// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::{CapabilitySet, ExecutableAvailability as A, ProviderKind, capability};
use koru_session_runtime::providers::{
    AgentCapabilities, CompatibilityProbe, DelegationFacts, ManagedProvider,
    ManagedProviderFactory, ManagedProviderRequest, ProbeLimits, ProviderDiagnostic,
    ProviderFailure, ProviderReadinessCache, ProviderStage,
};

struct ProbeFactory {
    receive: Mutex<VecDeque<std::sync::mpsc::Receiver<Result<AgentCapabilities, ProviderFailure>>>>,
    started: std::sync::mpsc::Sender<()>,
}
impl ManagedProviderFactory for ProbeFactory {
    fn create(&self, _: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        unreachable!("readiness never creates a session")
    }
    fn compatibility_probe(
        &self,
        _: OsString,
        _: PathBuf,
    ) -> Result<Box<dyn CompatibilityProbe>, ProviderFailure> {
        Ok(Box::new(Probe {
            receive: self.receive.lock().unwrap().pop_front().unwrap(),
            started: self.started.clone(),
        }))
    }
}
struct Probe {
    receive: std::sync::mpsc::Receiver<Result<AgentCapabilities, ProviderFailure>>,
    started: std::sync::mpsc::Sender<()>,
}
impl CompatibilityProbe for Probe {
    fn provider(&self) -> ProviderKind {
        "fixture".parse().unwrap()
    }
    fn raw_available(&self) -> bool {
        false
    }
    fn probe(&self, _: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        self.started.send(()).unwrap();
        self.receive.recv_timeout(Duration::from_secs(5)).unwrap()
    }
}

fn host(registry: ManagedProviderRegistry) -> Arc<HostState> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("unused-test-root");
    let paths = koru_session_paths::SessionPaths::from_resolved_directory(root.clone());
    let registry = Arc::new(registry);
    let catalog = ProviderCatalogCache::new(
        Arc::clone(&registry),
        root.clone(),
        Box::new(koru_session_runtime::providers::MemoryCatalogStore::default()),
        false,
    );
    Arc::new(HostState {
        registry: SessionRegistry::new(SessionRuntimeConfig::default()),
        providers: ProviderReadinessCache::with_catalog(
            Arc::clone(&registry),
            root.clone(),
            true,
            Arc::clone(&catalog),
        ),
        catalog,
        projects: ProjectRegistry::in_memory(&paths),
        attachments: Arc::new(
            koru_session_attachments::AttachmentStore::new(
                paths,
                koru_session_attachments::AttachmentLimits::default(),
            )
            .unwrap(),
        ),
        instance_id: "fixture".into(),
        bootstrap: LaunchBootstrap::new(
            "fixture".into(),
            "dev".into(),
            root.clone(),
            root.clone(),
            root,
            true,
            "fixture-authority".into(),
        ),
        started: Instant::now(),
        next_subscription: AtomicU64::new(1),
        control_slots: Arc::new(Semaphore::new(16)),
        model_asks: Arc::new(Semaphore::new(MODEL_ASKS)),
        create_replays: Mutex::new(VecDeque::new()),
        stop: Notify::new(),
        bridge: None,
        tab_bridge_files: None,
    })
}

async fn open(
    state: Arc<HostState>,
    readiness: bool,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<Result<(), HostError>>,
) {
    let (host, mut peer) = tokio::io::duplex(65536);
    let task = tokio::task::spawn_local(serve_connection(host, state));
    let mut offer = crate::protocol_offer();
    if !readiness {
        offer.capabilities = CapabilitySet::new(
            offer
                .capabilities
                .iter()
                .filter(|value| value.as_str() != capability::PROVIDER_READINESS)
                .cloned(),
        );
    }
    write_frame(
        &mut peer,
        &koru_session_wire::ClientHello {
            product: "readiness-fixture".into(),
            product_version: "fixture".into(),
            offer,
            role: None,
        },
    )
    .await
    .unwrap();
    let (_, greeting) = read_frame::<_, Greeting>(&mut peer).await.unwrap();
    assert!(matches!(greeting, Greeting::Welcome { .. }));
    (peer, task)
}

async fn request(peer: &mut tokio::io::DuplexStream, id: u64, request: Request) -> Response {
    write_frame(peer, &RequestFrame { id, request })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, frame) = read_frame::<_, FromHost>(peer).await.unwrap();
            if let FromHost::Response { frame } = frame
                && frame.id == id
            {
                return frame.result.unwrap();
            }
        }
    })
    .await
    .expect("ordinary dispatch must not wait on a provider")
}

fn capabilities() -> AgentCapabilities {
    AgentCapabilities {
        provider: "fixture".parse().unwrap(),
        executable_version: "1".parse().unwrap(),
        protocol_version: "1".parse().unwrap(),
        capabilities: Default::default(),
        delegation: DelegationFacts::undeclared(),
    }
}

#[tokio::test]
async fn cold_subscription_slow_probe_and_retry_use_the_real_host_dispatch() {
    tokio::task::LocalSet::new().run_until(async {
        let (first_send, first_receive) = std::sync::mpsc::channel();
        let (second_send, second_receive) = std::sync::mpsc::channel();
        let (started, starts) = std::sync::mpsc::channel();
        let mut registry = ManagedProviderRegistry::default();
        registry.register(koru_session_profiles::ManagedProviderRegistration {
            kind: "fixture", ..koru_session_profiles::CODEX_REGISTRATION
        }, Arc::new(ProbeFactory {
            receive: Mutex::new(VecDeque::from([first_receive, second_receive])), started,
        })).unwrap();
        let state = host(registry);
        let (mut peer, task) = open(state.clone(), true).await;
        let snapshot = request(&mut peer, 1, Request::SubscribeProviders).await;
        assert!(matches!(snapshot, Response::ProvidersSubscribed { providers, .. }
            if providers.len() == 1 && providers[0].availability == A::Checking));
        assert!(starts.try_recv().is_err(), "a cached subscription started a provider");
        assert_eq!(request(&mut peer, 2, Request::RefreshProvider { kind: "fixture".parse().unwrap() }).await, Response::Ok);
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(request(&mut peer, 3, Request::ListSessions).await, Response::Sessions { sessions } if sessions.is_empty()));
        assert_eq!(request(&mut peer, 4, Request::RefreshProvider { kind: "fixture".parse().unwrap() }).await, Response::Ok);
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        second_send.send(Ok(capabilities())).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (_, frame) = read_frame::<_, FromHost>(&mut peer).await.unwrap();
                if let FromHost::Subscription { frame: SubscriptionFrame {
                    update: SubscriptionUpdate::Providers { provider }, ..
                }} = frame && matches!(provider.availability, A::Available { .. }) { break; }
            }
        }).await.unwrap();
        first_send.send(Err(ProviderFailure::new(ErrorCode::ProviderUnavailable,
            ProviderStage::Initialize, ProviderDiagnostic::AuthenticationRequired))).unwrap();
        assert!(matches!(request(&mut peer, 5, Request::ListSessions).await, Response::Sessions { .. }));
        drop(peer);
        task.await.unwrap().unwrap();
    }).await;
}

/// A factory whose list is made when asked, counting each listing. It has
/// no probe, so a re-probe settles at once as a failure.
struct ListingFactory {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
impl ManagedProviderFactory for ListingFactory {
    fn create(&self, _: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        unreachable!("listing never creates a session")
    }
    fn compatibility_probe(
        &self,
        _: OsString,
        _: PathBuf,
    ) -> Result<Box<dyn CompatibilityProbe>, ProviderFailure> {
        Ok(Box::new(ReadyProbe))
    }
    fn discover_models(
        &self,
        _: OsString,
        _: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(vec![koru_session_protocol::ProviderModel::new(
            "model".parse().unwrap(),
            "fixture".parse().unwrap(),
        )]
        .try_into()
        .unwrap())
    }
}

/// A probe that finds the agent ready at once.
struct ReadyProbe;
impl CompatibilityProbe for ReadyProbe {
    fn provider(&self) -> ProviderKind {
        "fixture".parse().unwrap()
    }
    fn raw_available(&self) -> bool {
        false
    }
    fn probe(&self, _: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        Ok(capabilities())
    }
}

/// `RefreshProvider` drops the agent's model list with its readiness, so
/// the next answer is a list made again, not the one from before (D6, D7,
/// D13 all arrive here). Readiness drives it: each refresh re-probes, and
/// the catalog lists once the agent reads ready.
#[tokio::test]
async fn refresh_provider_drops_the_agents_model_list() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut registry = ManagedProviderRegistry::default();
            registry
                .register(
                    koru_session_profiles::ManagedProviderRegistration {
                        kind: "fixture",
                        ..koru_session_profiles::CODEX_REGISTRATION
                    },
                    Arc::new(ListingFactory {
                        calls: calls.clone(),
                    }),
                )
                .unwrap();
            let state = host(registry);
            let kind: ProviderKind = "fixture".parse().unwrap();
            let listings = || calls.load(std::sync::atomic::Ordering::SeqCst);
            let (mut peer, task) = open(state.clone(), true).await;
            for (id, expected) in [(1, 1), (2, 2)] {
                assert_eq!(
                    request(
                        &mut peer,
                        id,
                        Request::RefreshProvider { kind: kind.clone() }
                    )
                    .await,
                    Response::Ok
                );
                let catalog = Arc::clone(&state.catalog);
                let asked = kind.clone();
                let answers = tokio::task::spawn_blocking(move || {
                    (catalog.serve(&asked).is_ok(), catalog.serve(&asked).is_ok())
                })
                .await
                .unwrap();
                assert_eq!(answers, (true, true));
                assert_eq!(
                    listings(),
                    expected,
                    "each refresh drops the list; the second ask is from memory"
                );
            }
            drop(peer);
            task.await.unwrap().unwrap();
        })
        .await;
}

#[tokio::test]
async fn an_old_client_gets_no_new_updates_and_its_connection_survives() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = host(ManagedProviderRegistry::default());
            let (mut peer, task) = open(state, false).await;
            assert!(matches!(
                request(&mut peer, 1, Request::ListSessions).await,
                Response::Sessions { .. }
            ));
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(20),
                    read_frame::<_, FromHost>(&mut peer)
                )
                .await
                .is_err()
            );
            assert!(matches!(
                request(&mut peer, 2, Request::ListSessions).await,
                Response::Sessions { .. }
            ));
            drop(peer);
            task.await.unwrap().unwrap();
        })
        .await;
}
