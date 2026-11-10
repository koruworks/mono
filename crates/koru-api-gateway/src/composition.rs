// SPDX-License-Identifier: MPL-2.0

//! What the gateway is made of: the Store worker, the events worker, the AI
//! worker, the Artifact Store, and the settings authority handed down before
//! anything is served.
//!
//! **This is the composition, unchanged.** Which constructor is called, and in
//! which order the workers open, is what a rehost is most likely to alter by
//! accident, so the whole of it lives in one function that a second host calls
//! rather than reproduces.
//!
//! Two properties are load-bearing and easy to lose:
//!
//! - **Optional services degrade truthfully.** A journal that will not open
//!   leaves `/v1/events` answering 503; an AI worker that will not spawn falls
//!   back to a Store-only composition whose capabilities carry `ai-disabled`;
//!   an `artifacts.db` that will not open leaves `/v1/artifacts` answering
//!   503 with no `artifacts` capability. None of them keeps the Store closed.
//! - **The account domain is deliberately absent.** The account service is
//!   the hosted Koru Cloud's (not in this repository), and the gateway has no
//!   slot for one: its account routes exist and answer 503
//!   `capability_unavailable`, and no `account` capability is published.
//!   `the_desktop_host_publishes_no_account_capability` in
//!   `apps/koru-agent/tests/gateway_host_parity.rs` is what fails if that
//!   changes.

mod settings;

use std::path::Path;
use std::sync::Arc;

use koru_api_gateway_core::GatewayState;

use crate::lifecycle::GatewayConfig;

/// The composed runtime, and the two tasks that pump the Agent's network edge.
pub(crate) struct Composition {
    pub(crate) state: Arc<GatewayState>,
    /// The forwarders that carry the Agent bridge's accepted sessions and
    /// connection events into the Store worker. Aborted with the router.
    pub(crate) bridge_forwarders:
        Option<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)>,
}

/// Open every worker and hand the settings authority down.
///
/// **Called from inside the runtime**, because the bridge forwarders are
/// `tokio::spawn`ed. It performs no argument parsing and never exits the
/// process: a worker that must be there and is not becomes an `Err`.
pub(crate) fn open(
    config: &GatewayConfig,
    store_root: &Path,
    bridge: Option<koru_network_bridge::wire::Link>,
    sessions: Option<koru_api_gateway_core::SessionBridge>,
) -> Result<Composition, String> {
    let segment = config.build.segment.as_str();
    let ai_root = config.channel_root.join("ai");
    // Sibling of a conventional store/ directory, inside a bare test root —
    // koru-channel owns the rule. The old inside-the-root location is passed
    // as the legacy dir so an existing journal is adopted by rename.
    let events_root = koru_channel::events_root_beside(store_root);
    // The Artifact Store's directory, by the same rule (D13): beside `ai/`
    // and `events/`, and under `--root` with everything else.
    let artifacts_root = koru_channel::artifacts_root_beside(store_root);
    let legacy_events_root = store_root.join("events");

    let (worker_config, bridge_forwarders) = match bridge {
        Some(bridge) => {
            let (sessions_tx, sessions_rx) = tokio::sync::mpsc::channel(32);
            let (connections_tx, connections_rx) = tokio::sync::mpsc::channel(32);
            let session_link = bridge.clone();
            let session_forwarder = tokio::spawn(async move {
                while let Some(session) = session_link.accepted().await {
                    if sessions_tx.send(session).await.is_err() {
                        break;
                    }
                }
            });
            let connection_link = bridge.clone();
            let connection_forwarder = tokio::spawn(async move {
                while let Some(event) = connection_link.connection_event().await {
                    if connections_tx.send(event).await.is_err() {
                        break;
                    }
                }
            });
            (
                koru_store_server::WorkerConfig::embedded_with_network_edge(
                    store_root.to_path_buf(),
                    segment,
                    Arc::new(bridge),
                    sessions_rx,
                    connections_rx,
                ),
                Some((session_forwarder, connection_forwarder)),
            )
        }
        None => (
            koru_store_server::WorkerConfig::embedded(store_root.to_path_buf(), segment),
            None,
        ),
    };

    // The events worker, beside the Store worker. A journal that will not
    // open is a degradation, not a refusal: the store must not stay closed
    // because observations cannot be recorded, so this logs and carries on
    // with `/v1/events` answering 503.
    let events = match koru_events_server::EventsHandle::open(
        koru_events_server::WorkerConfig::new(events_root).with_legacy_dir(legacy_events_root),
    ) {
        Ok(events) => Some(events),
        Err(error) => {
            tracing::error!(%error, "the events worker could not open; /v1/events is unavailable");
            None
        }
    };
    let worker_config = match events.clone() {
        Some(events) => worker_config.with_events(events),
        None => worker_config,
    };
    let worker = koru_store_server::StoreWorker::new(worker_config)
        .spawn()
        .map_err(|error| error.to_string())?;
    let store_client = worker.client();
    let store_context = Arc::new(koru_ai_server::StoreContextAdapter::new(
        store_client.clone(),
    ));
    let tools = Arc::new(koru_ai_server::AiToolDispatcher::new(
        koru_ai_server::StoreToolAdapter::new(store_client.clone()),
        koru_ai_server::ChatToolAdapter::new(ai_root.clone()),
    ));
    let ai_config = if config.deterministic_ai_test {
        Ok(koru_ai_server::WorkerConfig::deterministic_mock_with_delay(
            ai_root,
            segment,
            std::time::Duration::from_millis(20),
        )
        .with_store_context(store_context)
        .with_tool_executor(tools.clone()))
    } else {
        koru_ai_server::WorkerConfig::production(ai_root, segment)
            .map(|worker_config| {
                let worker_config = worker_config
                    .with_store_context(store_context)
                    .with_tool_executor(tools);
                // Local agents drive the session host over its private,
                // root-qualified endpoint — the same rule the session
                // bridge follows. A host that is not up costs nothing
                // until a person picks an agent.
                match koru_ipc::Endpoint::for_service_channel_root(
                    koru_ipc::SESSION_HOST,
                    segment,
                    &config.channel_root,
                ) {
                    Ok(endpoint) => worker_config.with_session_host(endpoint),
                    Err(error) => {
                        tracing::warn!(%error, "local agents are unavailable: no session host endpoint");
                        worker_config
                    }
                }
            })
            .map_err(|error| error.public_message)
    };
    // One private capability, composed independently of AI startup. Discovery
    // must also work while an AI worker is unavailable; effects still require it.
    let bot_delegate: Option<Arc<dyn koru_delegate_api::bots::BotDelegatePort>> =
        config.bootstrap.as_ref().and_then(|bootstrap| {
            let authority = koru_delegate_api::bots::BotDelegateAuthority::new(
                bootstrap.bot_delegate.as_ref()?.expose().to_owned(),
            )?;
            let endpoint = koru_ipc::Endpoint::for_service_channel_root(
                koru_ipc::DELEGATE,
                segment,
                &config.channel_root,
            )
            .ok()?;
            Some(Arc::new(koru_delegate_api::bots::LocalBotDelegate::new(
                endpoint, authority,
            ))
                as Arc<dyn koru_delegate_api::bots::BotDelegatePort>)
        });
    let native_mutations = Arc::new(koru_ai_server::NativeMutationGate::closed());
    let ai_config = ai_config.map(|ai_config| {
        let ai_config =
            ai_config.with_native_effect_recovery(store_client.clone(), native_mutations.clone());
        match config.bootstrap.as_ref().and_then(|b| {
            koru_ai_server::BotAuthorityAdapter::from_device(
                store_client.clone(),
                b.device_id.as_deref()?,
            )
        }) {
            Some(authority) => {
                let ai_config = ai_config.with_bot_authority(Arc::new(authority));
                match bot_delegate.clone() {
                    Some(delegate) => ai_config.with_bot_delegate(delegate),
                    None => ai_config,
                }
            }
            None => ai_config,
        }
    });
    let state = match ai_config.and_then(|ai_config| {
        koru_ai_server::AiWorker::new(ai_config)
            .spawn()
            .map_err(|error| error.to_string())
    }) {
        Ok(ai_worker) => GatewayState::with_ai_events_and_services(
            segment.to_owned(),
            worker,
            ai_worker,
            events,
            sessions,
        ),
        Err(error) => {
            tracing::error!(%error, "Koru AI worker unavailable; Store remains ready");
            GatewayState::new_with_services(segment.to_owned(), worker, events, sessions)
        }
    };

    if let Some(delegate) = bot_delegate {
        state.set_bot_delegate_port(delegate);
    }
    state.set_native_mutation_gate(native_mutations);

    // The Artifact Store, hosted here (D13) with its own database. Opening
    // it is a migration from empty on the first run and a stamp check on
    // every later one; a database that will not open — stamped by a newer
    // build, say — is a degradation, not a refusal, for the journal's
    // reason. The routes then answer 503 and no capability is published.
    match koru_artifact_sqlite::Repository::open(&artifacts_root) {
        Ok(repository) => {
            state.set_artifact_service(Arc::new(koru_artifact_api::Service::new(repository)));
        }
        Err(error) => {
            tracing::error!(%error, "artifacts.db could not open; /v1/artifacts is unavailable");
        }
    }

    // The delegate transport (D12): this device's worker over its private,
    // root-qualified endpoint. Composed whether or not the worker is up —
    // the Agent starts it after this gateway — so the routes exist from the
    // first request and answer 503 until it answers. A host with no Agent
    // (a rooted gateway harness) composes it too, and the routes say the
    // worker is not answering, which is the truth.
    let delegate_endpoint = if config.bootstrap.is_some()
        || koru_channel::channel_root().as_deref() != Some(config.channel_root.as_path())
    {
        koru_ipc::Endpoint::for_service_channel_root(
            koru_ipc::DELEGATE,
            segment,
            &config.channel_root,
        )
    } else {
        koru_ipc::Endpoint::for_service(koru_ipc::DELEGATE, segment)
    };
    match delegate_endpoint {
        Ok(endpoint) => {
            state.set_delegate_port(Arc::new(koru_delegate_api::LocalDelegate::new(
                endpoint,
                "koru-api-gateway",
            )));
        }
        Err(error) => {
            tracing::error!(%error, "the delegate endpoint could not be named; /v1/runs is unavailable");
        }
    }

    let settings_fault = std::env::var(koru_env::name::KORU_INTERNAL_SETTINGS_WRITE_FAULT).ok();
    let settings_fault = state.configure_settings_write_fault(
        koru_channel::ReleaseChannel::this_build_is_internal(),
        settings_fault.as_deref(),
    );
    report_settings_fault(settings_fault);

    settings::install_authority(&config.channel_root, &state);

    Ok(Composition {
        state,
        bridge_forwarders,
    })
}

fn report_settings_fault(configuration: koru_api_gateway_core::settings::FaultConfiguration) {
    use koru_api_gateway_core::settings::FaultConfiguration;

    match configuration {
        FaultConfiguration::Off | FaultConfiguration::Armed => {}
        FaultConfiguration::RefusedReleasedBuild => {
            tracing::warn!("the internal settings write fault was ignored by this released build")
        }
        FaultConfiguration::UnknownOperation => tracing::warn!(
            "the internal settings write fault named an unknown operation and was ignored"
        ),
    }
}
