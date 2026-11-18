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
use koru_network_edge::TransportSession;

use crate::bot_ingress::BotIngress;
use crate::lifecycle::GatewayConfig;

/// Whether Bots cross between this person's linked computers at all (M8,
/// P8.3). **One switch for both directions**, read in one place,
/// [`connect_linked_bots`]:
///
/// - **serving**: a linked device may read and steer this computer's Bots.
///   While `false` the owner server is built and tested, and is not
///   installed: the adapter answers every linked device `bots_unavailable`;
/// - **following**: this computer keeps a copy of the Bots its linked
///   computers run and sends them commands. While `false` no mirror is
///   started, so this computer never dials `koru/bots/1`, never opens a Bot
///   stream to another device and writes nothing to the copy tables, and
///   the two linked reads answer "no linked Bots".
///
/// **Opened by the product owner on 2026-11-17**, to test it on two
/// desktops (P8.4), together with the app's `COPIES_DISCLOSED`.
/// `m8_linked_switch_is_open_and_the_composition_serves_and_dials` pins it.
pub(crate) const LINKED_BOT_CONTROL_AVAILABLE: bool = true;

/// Open both directions of Bots on linked computers for one AI worker,
/// **only when [`LINKED_BOT_CONTROL_AVAILABLE`] is `true`**: install the
/// worker's owner server behind the Bot ingress, and start the worker's
/// mirror over the bridge link, which is the Bot client's egress as it is
/// the Store worker's. With the switch `false` this does nothing at all.
///
/// Called before the gateway state is built, so the state's AI client
/// routes by owner when, and only when, a mirror was started.
pub(crate) fn connect_linked_bots(
    ai_worker: &koru_ai_server::WorkerHandle,
    ingress: Option<&BotIngress>,
    egress: Option<Arc<dyn koru_network_edge::NetworkEgress>>,
) {
    if !LINKED_BOT_CONTROL_AVAILABLE {
        return;
    }
    // With no device network there is neither an ingress nor an egress.
    let Some(ingress) = ingress else {
        return;
    };
    // A worker with no Bot authority has no owner server, and the adapter
    // keeps answering `bots_unavailable`.
    if let Some(server) = ai_worker.bot_owner_server() {
        ingress.set_owner_server(Arc::new(crate::bot_ingress::AiOwnerServer(server)));
    }
    if let Some(egress) = egress {
        let _ = ai_worker.start_bot_mirror(egress, ingress.peers());
    }
}

/// The composed runtime, and the two tasks that pump the Agent's network edge.
pub(crate) struct Composition {
    pub(crate) state: Arc<GatewayState>,
    /// The forwarders that carry the Agent bridge's accepted sessions and
    /// connection events into the Store worker. Aborted with the router.
    pub(crate) bridge_forwarders:
        Option<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)>,
    /// Where a linked device's Bot streams arrive (M8), when there is a
    /// device network at all. [`open`] installs the AI worker's owner server
    /// in it when that worker serves Bots; otherwise the adapter refuses
    /// every stream.
    #[allow(dead_code)]
    pub(crate) bot_ingress: Option<BotIngress>,
}

/// Where one session the Agent's bridge accepted goes, by its token.
///
/// `koru/bots/1` goes to the Bot ingress adapter. **Everything else goes to
/// the Store worker, exactly as before M8.** The order matters: the Store
/// worker resets any session that is not replication, so a Bot session must
/// be taken out before it gets there.
///
/// Answers `false` when the Store worker is gone and the forwarder should
/// stop.
pub(crate) async fn route_accepted_session(
    session: TransportSession,
    store: &tokio::sync::mpsc::Sender<TransportSession>,
    bots: &BotIngress,
) -> bool {
    if session.protocol.as_str() == koru_bot_client_wire::PROTOCOL_TOKEN {
        bots.admit(session);
        return true;
    }
    store.send(session).await.is_ok()
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
    let ai_root = koru_channel::ai_root_under(&config.channel_root);
    // Sibling of a conventional store/ directory, inside a bare test root —
    // koru-channel owns the rule. The old inside-the-root location is passed
    // as the legacy dir so an existing journal is adopted by rename.
    let events_root = koru_channel::events_root_beside(store_root);
    // The Artifact Store's directory, by the same rule (D13): beside `ai/`
    // and `events/`, and under `--root` with everything else.
    let artifacts_root = koru_channel::artifacts_root_beside(store_root);
    let legacy_events_root = store_root.join("events");

    // The bridge link is also where this computer opens a Bot stream to a
    // linked device. Nothing opens one unless the switch is open, which it
    // is in this build.
    let bot_egress = bridge
        .as_ref()
        .map(|bridge| Arc::new(bridge.clone()) as Arc<dyn koru_network_edge::NetworkEgress>);
    let (worker_config, bridge_forwarders, bot_ingress) = match bridge {
        Some(bridge) => {
            let (sessions_tx, sessions_rx) = tokio::sync::mpsc::channel(32);
            let (connections_tx, connections_rx) = tokio::sync::mpsc::channel(32);
            // The bridge link is the Store worker's egress and, for the Bot
            // client, the same link is its egress too.
            let worker_config = koru_store_server::WorkerConfig::embedded_with_network_edge(
                store_root.to_path_buf(),
                segment,
                Arc::new(bridge.clone()),
                sessions_rx,
                connections_rx,
            );
            // The Bot admission port is the Store worker's: it is fed from
            // the authorization snapshots the worker's sync control session
            // receives. The adapter is made before the first session can be
            // forwarded, so a Bot stream never meets a gateway that would
            // hand it to the Store worker.
            let bot_ingress = BotIngress::new(worker_config.bot_peers());
            let session_link = bridge.clone();
            let session_bots = bot_ingress.clone();
            let session_forwarder = tokio::spawn(async move {
                while let Some(session) = session_link.accepted().await {
                    if !route_accepted_session(session, &sessions_tx, &session_bots).await {
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
                worker_config,
                Some((session_forwarder, connection_forwarder)),
                Some(bot_ingress),
            )
        }
        None => (
            koru_store_server::WorkerConfig::embedded(store_root.to_path_buf(), segment),
            None,
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
    // The AI worker's Bot events (M8) go to the same journal, through a
    // queue of their own so the AI writer never waits for it. Without an
    // events worker nothing is published. The Bot ingress adapter publishes
    // the streams it turns away to the same sink.
    let bot_events = events
        .clone()
        .and_then(crate::bot_events::BotEventJournal::start)
        .map(|journal| journal as Arc<dyn koru_ai_server::BotEventSink>);
    if let (Some(ingress), Some(sink)) = (&bot_ingress, &bot_events) {
        ingress.set_events(koru_ai_server::BotEvents::new(Arc::clone(sink)));
    }
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
                    .with_tool_executor(tools)
                    .with_owner_zone(Arc::new(owner_zone));
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
    let legacy_bots = koru_bots_engine::legacy_allowed(&config.channel_root)
        .map_err(|error| error.to_string())?;
    let bot_authority = config.bootstrap.as_ref().and_then(|bootstrap| {
        koru_delegate_api::bots::BotDelegateAuthority::new(
            bootstrap.bot_delegate.as_ref()?.expose().to_owned(),
        )
    });
    let routine_bridge_authority = bot_authority
        .as_ref()
        .filter(|_| legacy_bots)
        .map(|authority| authority.routine_bridge_authority());
    // The decision bridge's own domain: same bootstrap parent, separate derive
    // context and header scheme, so neither credential opens the other's routes.
    let decision_bridge_authority = bot_authority
        .as_ref()
        .filter(|_| legacy_bots)
        .map(|authority| authority.decision_bridge_authority());
    let bot_delegate: Option<Arc<dyn koru_delegate_api::bots::BotDelegatePort>> = bot_authority
        .and_then(|authority| {
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
    // The admission port the Bot ingress adapter reads is the one the AI
    // worker's owner server reads again for every message of a linked
    // device (M8). With no device network there is neither.
    let bot_peers = bot_ingress.as_ref().map(BotIngress::peers);
    let ai_config = ai_config.map(|ai_config| {
        let ai_config = ai_config
            .with_native_effect_recovery(store_client.clone(), native_mutations.clone())
            .with_store_root(store_root.to_path_buf());
        let ai_config = match bot_peers {
            Some(peers) => ai_config.with_bot_peers(peers),
            None => ai_config,
        };
        let ai_config = match bot_events {
            Some(sink) => ai_config.with_bot_events(sink),
            None => ai_config,
        };
        match config.bootstrap.as_ref().and_then(|b| {
            koru_ai_server::BotAuthorityAdapter::from_device(
                store_client.clone(),
                b.device_id.as_deref()?,
            )
        }) {
            Some(authority) => {
                let ai_config = ai_config.with_bot_authority(Arc::new(authority));
                match bot_delegate.clone() {
                    // One IPC16 read through the same private port serves the
                    // AI decision owner's wait and resume reads.
                    Some(delegate) => ai_config
                        .with_decision_wait_delegate(Arc::new(
                            koru_ai_server::BotDelegateDecisionWait::new(delegate.clone()),
                        ))
                        .with_bot_delegate(delegate),
                    None => ai_config,
                }
            }
            None => ai_config,
        }
    });
    let state = match ai_config.and_then(|ai_config| {
        let ai_config = if koru_bots_engine::legacy_allowed(&config.channel_root)
            .map_err(|error| error.to_string())?
        {
            ai_config
        } else {
            ai_config.without_legacy_bots()
        };
        koru_ai_server::AiWorker::new(ai_config)
            .spawn()
            .map_err(|error| error.to_string())
    }) {
        Ok(ai_worker) => {
            // Bots on linked computers (M8), both directions, behind one
            // switch, open in this build (the product owner, 2026-11-17):
            // the owner server is installed and the mirror is started.
            connect_linked_bots(&ai_worker, bot_ingress.as_ref(), bot_egress);
            GatewayState::with_ai_events_and_services(
                segment.to_owned(),
                worker,
                ai_worker,
                events,
                sessions,
            )
        }
        Err(error) => {
            tracing::error!(%error, "Koru AI worker unavailable; Store remains ready");
            GatewayState::new_with_services(segment.to_owned(), worker, events, sessions)
        }
    };

    if let Some(delegate) = bot_delegate {
        state.set_bot_delegate_port(delegate);
    }
    if let Some(authority) = routine_bridge_authority {
        state.set_routine_bridge_authority(authority);
    }
    if let Some(authority) = decision_bridge_authority {
        state.set_decision_bridge_credential(Arc::new(authority));
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
        bot_ingress,
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

/// This computer's time zone name, as the delegate's scheduler detects it
/// (`SystemZone::detect`, UTC when the system does not say). Read on every
/// call and never cached, so a system zone change is seen by the next
/// Routine read (P4.3 controls contract, 4.8).
fn owner_zone() -> Option<String> {
    Some(
        koru_time_zone::SystemZone::detect()
            .rules_fingerprint()
            .canonical_zone,
    )
}
