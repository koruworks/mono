// SPDX-License-Identifier: MPL-2.0

//! Start the gateway, serve it, and stop it — in one place, in one order.
//!
//! [`start`] is the whole of what used to be `main.rs::run` below the argument
//! parse. It takes a [`GatewayConfig`] a host resolved, and hands back a
//! [`Gateway`]: a runtime that is composed, listening and published, whose
//! [`Gateway::readiness`] is the record a supervisor may report and whose
//! [`Gateway::serve`] runs until shutdown and answers with a [`Completion`].
//!
//! Nothing here reads `std::env::args` and nothing here calls `process::exit`.
//! A failure is an `Err(String)`; what that costs is the host's decision.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use koru_api_gateway_core::GatewayState;

use crate::{bootstrap, composition, discovery};

/// Which image is hosting this runtime, as the published record describes it.
///
/// Carried rather than compiled in, because the runtime must not answer "what
/// am I" out of its own `env!`: a second host publishes *its* version, and a
/// rooted rehearsal publishes the segment it was launched for.
#[derive(Clone, Debug)]
pub struct BuildIdentity {
    /// The release channel segment — `koru_channel::segment()` for a real host.
    /// It names the IPC endpoints, the Store's channel and the published
    /// `GatewayHello.segment`, so every one of those agrees by construction.
    pub segment: String,
    /// What `GatewayHello.gateway_version` says.
    pub gateway_version: String,
}

impl BuildIdentity {
    pub fn new(segment: impl Into<String>, gateway_version: impl Into<String>) -> Self {
        Self {
            segment: segment.into(),
            gateway_version: gateway_version.into(),
        }
    }
}

/// Everything a host resolved before a runtime existed.
///
/// **No `Debug`, and no `Clone`.** It carries the supervisor's bootstrap, which
/// holds the bridge capability and the session-lifecycle authority; a derived
/// `Debug` is how a secret reaches a log line by accident.
pub struct GatewayConfig {
    pub(crate) channel_root: PathBuf,
    pub(crate) build: BuildIdentity,
    pub(crate) bootstrap: Option<koru_network_bridge::LaunchBootstrap>,
    pub(crate) deterministic_ai_test: bool,
}

impl GatewayConfig {
    /// `channel_root` is **already resolved**. There is no fall-back inside the
    /// runtime and no guessed root: a host that could not determine one refuses
    /// before it gets here.
    pub fn new(channel_root: PathBuf, build: BuildIdentity) -> Self {
        Self {
            channel_root,
            build,
            bootstrap: None,
            deterministic_ai_test: false,
        }
    }

    /// The Agent launch bootstrap, when this gateway is supervised.
    pub fn with_bootstrap(
        mut self,
        bootstrap: Option<koru_network_bridge::LaunchBootstrap>,
    ) -> Self {
        self.bootstrap = bootstrap;
        self
    }

    /// Serve a deterministic mock AI provider. Set only by a debug host.
    pub fn with_deterministic_ai_test(mut self, deterministic_ai_test: bool) -> Self {
        self.deterministic_ai_test = deterministic_ai_test;
        self
    }

    pub fn channel_root(&self) -> &Path {
        &self.channel_root
    }

    /// The store's directory under the channel root. `koru-channel` owns the
    /// rule; this is the one place the runtime derives it.
    pub fn store_root(&self) -> PathBuf {
        koru_channel::store_root_under(&self.channel_root)
    }

    /// Whether an Agent is supervising this run. It is also what decides
    /// whether logging prints beside its file — an unsupervised run has a
    /// console to print to.
    pub fn is_supervised(&self) -> bool {
        self.bootstrap.is_some()
    }
}

/// Start this gateway's logging sink, and hold the returned guard for the
/// process.
///
/// **The host calls this, before [`start`].** Where the records go is a host
/// concern: a supervised child and its parent must not contend for one
/// destination, and only the host knows which it is. Holding the guard in the
/// host also means the last line — the one that says why the runtime refused to
/// start — is still written, which it was not when the guard was a local of the
/// function that returned the error.
pub fn init_logging(config: &GatewayConfig) -> koru_store_server::log::Logging {
    koru_store_server::log::init_gateway(&config.store_root(), !config.is_supervised())
}

/// What a supervisor may report once the readiness contract holds.
///
/// It exists because "ready" is not "the process is up": it is *composed,
/// listening, and discoverable*. Everything here was true at the moment
/// discovery began answering.
///
/// It carries no bearer and no bootstrap secret. Minting a credential is what
/// the rendezvous does per caller; a readiness record is not a credential.
#[derive(Clone, Debug)]
pub struct Readiness {
    /// The loopback HTTP address the published record names.
    pub api_endpoint: String,
    /// This run's correlation id, and the `instance_id` clients hold.
    pub instance_id: String,
    /// The supervisor's launch id, when one launched this gateway.
    pub supervisor_launch_id: Option<String>,
    /// Exactly what the published `GatewayHello` advertises — the truthful
    /// degradation is visible here, not only over the wire.
    pub capabilities: Vec<String>,
    /// Whether a session bridge was connected. `false` is a documented
    /// degradation, not a failure.
    pub sessions_available: bool,
}

/// **How the Store's shutdown finished.**
///
/// The Store's `Ack` already means committed; joining its worker proves the
/// thread that made those commits has finished. Nothing new is tracked — this
/// carries the `WorkerExit` of the join that always existed and used to be
/// thrown away by `WorkerHandle`'s destructor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreCompletion {
    /// Nobody joined the Store worker on this path, so nothing was observed.
    /// **Absence of a report is never evidence of a clean join**, and
    /// [`Completion::is_clean`] treats it as such: it is the answer for a
    /// second join of an already-joined handle, and for a teardown that never
    /// reached the join at all.
    #[default]
    NotObserved,
    /// The Store worker's thread was joined and this is what it answered.
    Joined(koru_store_server::WorkerExit),
}

impl std::fmt::Display for StoreCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotObserved => formatter.write_str("the Store's exit was not observed"),
            Self::Joined(koru_store_server::WorkerExit::Clean) => {
                formatter.write_str("the Store finished cleanly")
            }
            Self::Joined(koru_store_server::WorkerExit::Failed(error)) => {
                write!(formatter, "the Store finished with an error: {error}")
            }
            Self::Joined(koru_store_server::WorkerExit::Panicked) => {
                formatter.write_str("the Store worker panicked")
            }
        }
    }
}

/// What a completed shutdown is able to say about itself.
///
/// A value rather than `()`, deliberately: an update decides whether it may
/// replace an image from this, and a caller that was handed nothing has
/// nothing to fail closed on.
///
/// **Every teardown produces one, including the failed ones.** [`served`] is
/// where a serve error goes, so there is no exit path on which the Store's
/// join is skipped because something else went wrong first — which is exactly
/// the shape the previous `Result<Completion, String>` allowed.
///
/// [`served`]: Completion::served
#[derive(Debug)]
#[non_exhaustive]
pub struct Completion {
    /// Whether the HTTP server itself ended without an error.
    pub served: Result<(), String>,
    /// The AI worker's exit, when one was opened. `None` means the composition
    /// degraded to Store-only and there was nothing to join.
    pub ai: Option<koru_ai_server::WorkerExit>,
    /// See [`StoreCompletion`].
    pub store: StoreCompletion,
}

impl Completion {
    /// Whether the host may report a clean stop.
    ///
    /// Both halves have to hold: the server ended without an error, **and**
    /// the Store's thread was joined and answered `Clean`. An unobserved Store
    /// is not clean.
    pub fn is_clean(&self) -> bool {
        self.served.is_ok()
            && matches!(
                self.store,
                StoreCompletion::Joined(koru_store_server::WorkerExit::Clean)
            )
    }
}

impl std::fmt::Display for Completion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.served {
            Ok(()) => write!(formatter, "{}", self.store),
            Err(error) => write!(formatter, "the server stopped with {error}; {}", self.store),
        }
    }
}

/// A composed, listening, published gateway.
pub struct Gateway {
    state: Arc<GatewayState>,
    listener: tokio::net::TcpListener,
    discovery: tokio::task::JoinHandle<()>,
    bots_workspace: Option<crate::bots_workspace::Server>,
    bridge_forwarders: Option<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)>,
    readiness: Readiness,
}

/// Ask a running [`Gateway`] to stop serving, from outside its runtime.
///
/// **This is the seam a supervised host needs and the public API cannot
/// give it.** `POST /v1/shutdown` is an authenticated request from a client;
/// a worker role noticing that its parent has died is not a client, has no
/// bearer, and must not have to mint one to stop itself. It carries no
/// credential and no bootstrap secret — only the runtime's own notification.
#[derive(Clone)]
pub struct ShutdownRequest(Arc<tokio::sync::Notify>);

impl ShutdownRequest {
    /// Idempotent, and callable from any thread — including one blocked in a
    /// pipe read outside the runtime. The notification is *stored* when
    /// nothing is waiting yet, so a request that arrives between [`start`] and
    /// [`Gateway::serve`] still stops the gateway rather than being lost.
    pub fn request(&self) {
        self.0.notify_one();
    }
}

impl Gateway {
    /// What was true when discovery began answering.
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// A handle that asks this gateway to stop. Taken before
    /// [`Gateway::serve`], which consumes the gateway.
    pub fn shutdown_request(&self) -> ShutdownRequest {
        ShutdownRequest(Arc::clone(&self.state.shutdown))
    }

    /// Serve until something asks for shutdown, then stop the workers.
    ///
    /// **This is the one teardown, and it always runs to the end.** The order
    /// is the contract:
    ///
    /// 1. **Close the long-lived event tails, the instant shutdown is asked
    ///    for and before the server begins draining.** A tail is a response
    ///    body a client holds until *its* deadline — ninety seconds, in
    ///    `koru-api-client` — and a graceful HTTP drain waits for in-flight
    ///    responses. With one Koru window attached, a gateway asked to stop
    ///    took 90.1 s to exit, measured on this tree. A server that cannot
    ///    stop because a client is idling on a tail is the defect, so the
    ///    tails are *closed*, not waited on. Nothing is lost: the cursor is
    ///    the protocol and the client resumes.
    /// 2. Drain the server, stop being discoverable, stop the forwarders.
    /// 3. Ask the workers to stop, join AI, then **join the Store** — the join
    ///    that already existed, taken deliberately instead of left to a
    ///    destructor that discards what it finds.
    ///
    /// A serve error does not skip any of it: it is carried in
    /// [`Completion::served`] rather than returned, so no exit path can end
    /// without the Store having been joined and reported.
    pub async fn serve(self) -> Completion {
        let Gateway {
            state,
            listener,
            discovery,
            bots_workspace,
            bridge_forwarders,
            readiness: _,
        } = self;
        let shutdown = Arc::clone(&state.shutdown);
        let closing = Arc::clone(&state);
        let served = axum::serve(listener, koru_api_gateway_core::router(Arc::clone(&state)))
            .with_graceful_shutdown(async move {
                shutdown.notified().await;
                // Step 1, inside the shutdown signal itself: by the time axum
                // starts waiting for in-flight responses, the tails are
                // already ending.
                closing.close_event_streams();
            })
            .await
            .map_err(|error| error.to_string());
        discovery.abort();
        if let Some(server) = bots_workspace {
            server.stop().await;
        }
        if let Some((sessions, connections)) = bridge_forwarders {
            sessions.abort();
            connections.abort();
        }
        state.stop_workers();
        let ai = state.join_ai();
        let store = state
            .join_store()
            .map_or(StoreCompletion::NotObserved, StoreCompletion::Joined);
        Completion { served, ai, store }
    }
}

/// Compose, claim the root, open the workers, listen, and publish.
///
/// **Called from inside a runtime**, and never before one exists — which is
/// exactly why [`crate::load_dotenv`] is not a step in here.
///
/// The order below is the contract. Three parts of it exist for a reason the
/// code states, because each is easy to reverse and none of them fails loudly
/// when it is:
///
/// 1. **The private bridges connect before the rendezvous binds** (ordering 3),
///    so a child is never discoverable before the Agent's edge into it exists.
/// 2. **The root-qualified rendezvous binds before Store opens** (ordering 2),
///    so two gateway images cannot race into two owners for one root.
/// 3. **Discovery is served last**, once the composition is up and the listener
///    has an address, so a published record always names an endpoint that
///    answers.
///
/// Ordering (1) — the `.env` load before any thread or runtime exists — is the
/// one this function cannot hold, and is the host's: see [`crate::load_dotenv`].
pub async fn start(config: GatewayConfig) -> Result<Gateway, String> {
    let store_root = config.store_root();

    bootstrap::validate(
        &store_root,
        config.bootstrap.as_ref(),
        &config.build.segment,
    )?;

    // **Ordering (3): the private bridges, before the rendezvous is bound.**
    // A child that became discoverable first could be greeted by a client whose
    // vouched scope the Agent has no edge to validate.
    let bridge = match config.bootstrap.as_ref() {
        Some(bootstrap) => Some(
            bootstrap::connect_agent_bridge(&store_root, bootstrap, &config.build.segment).await?,
        ),
        None => None,
    };
    let sessions = match bootstrap::session_bridge_authority(config.bootstrap.as_ref()) {
        Some(authority) => Some(
            bootstrap::connect_session_bridge(
                &config.channel_root,
                &config.build.segment,
                authority,
            )
            .await?,
        ),
        None => None,
    };

    // **Ordering (2): bind the root-qualified public rendezvous before opening
    // Store.** Two gateway images therefore cannot race into two owners for one
    // root — and the loser refuses before it has opened a database.
    let rendezvous = discovery::bind(&config.build.segment, &config.channel_root)?;

    let composition::Composition {
        state,
        bridge_forwarders,
        // The session forwarder holds the Bot ingress adapter for as long as
        // it routes, and the composition already installed the AI worker's
        // owner server in it (M8), so this handle has no use here.
        bot_ingress: _,
    } = composition::open(&config, &store_root, bridge, sessions)?;

    // **The Store is open from here, so a refusal below joins it rather than
    // leaving it to a destructor** — the same rule [`Gateway::serve`] follows,
    // at the other end of the same boundary. Binding the listener is the only
    // fallible step left, and a bare `?` here would drop the join on the floor,
    // so it is grouped and its failure handled once.
    let listen = async {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let bots_workspace = crate::bots_workspace::start(&config, &state)?;
        Ok::<_, String>((listener, address, bots_workspace))
    };
    let (listener, address, bots_workspace) = match listen.await {
        Ok(listening) => listening,
        Err(error) => {
            state.stop_workers();
            let store = state
                .join_store()
                .map_or(StoreCompletion::NotObserved, StoreCompletion::Joined);
            tracing::error!(
                %error,
                completion = %store,
                "the Koru API Gateway could not start after its Store had opened"
            );
            return Err(error);
        }
    };
    let endpoint = format!("http://127.0.0.1:{}", address.port());
    // A run's authority names its endpoint in full (`<base>/v1/runs/<RunId>/mcp`),
    // and the base is known only now that the listener is bound.
    state.set_public_endpoint(endpoint.clone());
    tracing::info!(%endpoint, "Koru API Gateway listening");
    tracing::info!(docs = %format_args!("{endpoint}/docs"), "Koru API Gateway documentation ready");
    let instance_id = discovery::instance_id();
    // **The rooted Agent, when this gateway is rooted.** A rehearsal must ask
    // the supervisor that launched it, not somebody's installation; the same
    // rule the settings authority follows one screen up.
    let agent = koru_api_client::grants::agent_endpoint(
        &config.build.segment,
        (koru_channel::channel_root().as_deref() != Some(config.channel_root.as_path()))
            .then_some(config.channel_root.as_path()),
    )
    .ok();
    let readiness = Readiness {
        api_endpoint: endpoint.clone(),
        instance_id: instance_id.clone(),
        supervisor_launch_id: config
            .bootstrap
            .as_ref()
            .map(|bootstrap| bootstrap.launch_id.clone()),
        capabilities: state.public_capabilities(),
        sessions_available: state.sessions_available(),
    };
    let policy = Arc::new(discovery::Policy {
        state: Arc::clone(&state),
        segment: config.build.segment.clone(),
        gateway_version: config.build.gateway_version.clone(),
        endpoint,
        instance_id,
        supervisor_launch_id: readiness.supervisor_launch_id.clone(),
        agent,
    });

    // Published last: the readiness contract holds from here, and not before.
    let discovery = tokio::spawn(rendezvous.serve(policy));

    Ok(Gateway {
        state,
        listener,
        discovery,
        bots_workspace,
        bridge_forwarders,
        readiness,
    })
}
