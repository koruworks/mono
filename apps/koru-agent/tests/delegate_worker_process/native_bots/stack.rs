// SPDX-License-Identifier: MPL-2.0
//! Layered admission fixture; no production grant or executable impersonation.
use super::*;
use koru_api_discovery::{GatewayHello, GatewayResponse, SessionCredential};
use koru_api_gateway_core::{Admission, GatewayState, SessionBridge, now_millis};
use koru_delegate_api::DelegatePort;
use koru_session_protocol::{
    CapabilitySet, MAX_PROTOCOL_VERSION, ProtocolOffer, ProtocolRange, capability,
};
use std::sync::Arc;

struct ObservedBotPort(koru_delegate_api::bots::LocalBotDelegate);
impl koru_delegate_api::bots::BotDelegatePort for ObservedBotPort {
    fn execute(
        &self,
        command: koru_delegate_api::bots::BotCommand,
    ) -> Result<koru_delegate_api::bots::BotReply, koru_delegate_api::Failure> {
        use koru_delegate_api::bots::{BotCommand, BotReply};
        let phase = match &command {
            BotCommand::Reconcile { .. } => "reconcile",
            BotCommand::Observe { .. } => "observe",
            BotCommand::BeginTurn { .. } => "begin",
            BotCommand::Stop { .. } => "stop",
            // The automatic interruption is never a person's Stop; label it
            // apart so a diagnostic says which one reached the owner.
            BotCommand::Interrupt { .. } => "interrupt",
            BotCommand::IngestAttachment { .. } => "ingest",
            BotCommand::ListAttachments { .. } => "list-attachments",
            _ => "owner-command",
        };
        let observed_fence = match &command {
            BotCommand::Observe { fence, .. } => Some(fence.clone()),
            _ => None,
        };
        let result = self.0.execute(command);
        match &result {
            Err(failure) => eprintln!(
                "native owner {phase} refused: code={} retryable={}",
                failure.code, failure.retryable
            ),
            Ok(BotReply::Observation { observation }) => eprintln!(
                "native owner observe: turn_bound={} truncated={} events={}",
                observation.fence.turn.is_some(),
                observation.observation.earlier_truncated,
                observation.observation.events.len()
            ),
            Ok(BotReply::Admission { admission }) => eprintln!(
                "native owner {phase}: state={:?} fence_bound={}",
                admission.state,
                admission.fence.is_some()
            ),
            Ok(BotReply::Turn { .. }) => eprintln!("native owner {phase}: turn bound"),
            Ok(BotReply::Attachment { attachment }) => eprintln!(
                "native owner {phase}: status={:?} delivery={:?}",
                attachment.status, attachment.delivery_mode
            ),
            _ => {}
        }
        if let (Some(fence), Ok(BotReply::Observation { observation })) = (observed_fence, &result)
        {
            use koru_session_protocol::AgentEvent as E;
            for frame in &observation.observation.events {
                let (kind, turn) = match &frame.event {
                    E::CommandsUpdated { .. } => ("commands", None),
                    E::ContextUsage { .. } => ("context-usage", None),
                    E::ProviderState { .. } => ("provider-state", None),
                    E::TurnStarted { turn_id } => ("turn-started", Some(*turn_id)),
                    E::MessageDelta { turn_id, .. } => ("message", Some(*turn_id)),
                    E::ToolStarted { turn_id, .. } => ("tool-started", Some(*turn_id)),
                    E::ToolCompleted { turn_id, .. } => ("tool-completed", Some(*turn_id)),
                    E::ModeChanged { turn_id, .. } => ("mode", Some(*turn_id)),
                    E::Notice { turn_id, .. } => ("notice", Some(*turn_id)),
                    E::TurnCompleted { turn_id } => ("turn-completed", Some(*turn_id)),
                    E::TurnCancelled { turn_id, .. } => ("turn-cancelled", Some(*turn_id)),
                    E::TurnFailed { turn_id, .. } => ("turn-failed", Some(*turn_id)),
                    _ => ("other", None),
                };
                eprintln!(
                    "native observed frame: seq={} kind={kind} session_match={} turn_match={}",
                    frame.sequence.get(),
                    frame.session_id == fence.session,
                    turn.is_none() || turn == fence.turn
                );
            }
        }
        result
    }
}

pub(super) struct Worker {
    role: String,
    child: std::process::Child,
    pipe: Option<std::process::ChildStdin>,
}
impl Worker {
    fn start(
        root: &Path,
        role: &str,
        bootstrap: &impl serde::Serialize,
        path: &std::ffi::OsStr,
    ) -> Self {
        let mut command = Command::new(agent_exe());
        command
            .args(["--worker", role, "--root"])
            .arg(root)
            .env("PATH", path)
            .env("CODEX_FIXTURE_TURN", "native")
            .env(
                "CODEX_FIXTURE_NATIVE_CONFIG",
                root.join("native-config.json"),
            )
            .env("CODEX_FIXTURE_TRACE", root.join("native-fixture.trace"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if role == "delegate" {
            // The existing schedule-process fixture guard admits only this
            // copied Codex executable when provider probes are compiled out.
            command
                .env(
                    "DELEGATE_PROCESS_TEST_CLOCK",
                    root.join("delegate-process-test-clock"),
                )
                .env(
                    "DELEGATE_PROCESS_TEST_WAKE_AUTHORITY",
                    "delegate-process-test",
                );
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().expect("same-image worker");
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(bootstrap).unwrap())
            .unwrap();
        pipe.write_all(b"\n").unwrap();
        pipe.flush().unwrap();
        Self {
            role: role.into(),
            child,
            pipe: Some(pipe),
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.pipe.take());
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                eprintln!("native {} worker cleanup: exited", self.role);
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        if self.child.wait().is_ok() {
            eprintln!("native {} worker cleanup: terminated and reaped", self.role);
        } else {
            eprintln!("native {} worker cleanup: unverified", self.role);
        }
    }
}

fn offer() -> ProtocolOffer {
    ProtocolOffer {
        versions: ProtocolRange {
            min: MAX_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new(capability::ALL.iter().map(|name| name.parse().unwrap())),
    }
}

pub(super) struct Stack {
    pub client: koru_api_client::Client,
    pub sessions: koru_session_client::SessionClient,
    pub device: koru_delegate_core::DeviceId,
    catalog: Arc<catalog::Catalog>,
    state: Arc<GatewayState>,
    workers: Vec<Worker>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    runtime: tokio::runtime::Runtime,
    // Drop all database owners and runtime tasks before removing their root.
    pub root: tempfile::TempDir,
    _slot: StackSlot,
}
impl Stack {
    pub fn start() -> Self {
        let slot = StackSlot::acquire();
        let root = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        std::fs::write(
            root.path().join("delegate-process-test-clock"),
            now.to_string(),
        )
        .unwrap();
        let fixture = codex_fixture_path(root.path());
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(fixture).chain(std::env::split_paths(&inherited)))
                .unwrap();
        let store_root = koru_channel::store_root_under(root.path());
        let session_endpoint = koru_session_host::endpoint(segment(), Some(root.path())).unwrap();
        let session_bootstrap = koru_session_wire::LaunchBootstrap::new(
            uuid::Uuid::new_v4().to_string(),
            segment().into(),
            root.path().into(),
            root.path().join("agent/sessions"),
            store_root.clone(),
            true,
            uuid::Uuid::new_v4().to_string(),
        );
        let sessions_worker = Worker::start(root.path(), "sessions", &session_bootstrap, &path);
        let mut connected = None;
        wait_until("scripted session owner", PROMPT, || {
            connected = koru_session_client::SessionClient::connect(
                &session_endpoint,
                "native-bot-fixture",
                offer(),
            )
            .ok();
            connected.is_some()
        });
        let sessions = connected.unwrap();
        let device = koru_delegate_core::DeviceId::new();
        let authority =
            koru_delegate_api::bots::BotDelegateAuthority::new(uuid::Uuid::new_v4().to_string())
                .unwrap();
        let delegate_endpoint = koru_delegate_host::endpoint(segment(), Some(root.path())).unwrap();
        let bot_port = Arc::new(ObservedBotPort(
            koru_delegate_api::bots::LocalBotDelegate::new(
                delegate_endpoint.clone(),
                authority.clone(),
            ),
        ));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let (state, tasks, hello, catalog, native_mutations) = runtime.block_on(async {
            let (catalog_endpoint, catalog, catalog_task) =
                catalog::Catalog::start(root.path()).await;
            // Hold the same root-qualified rendezvous before opening Store.
            let rendezvous =
                koru_api_discovery::server::Rendezvous::bind_at(segment(), root.path()).unwrap();
            let events =
                koru_events_server::EventsHandle::open(koru_events_server::WorkerConfig::new(
                    koru_channel::events_root_beside(&store_root),
                ))
                .unwrap();
            let worker = koru_store_server::StoreWorker::new(
                koru_store_server::WorkerConfig::embedded(store_root.clone(), segment())
                    .with_events(events.clone()),
            )
            .spawn()
            .unwrap();
            let store = worker.client();
            // One process-scoped gate, composed as the gateway process does:
            // AI recovery opens it from its own startup sweep; the gateway
            // only reads it. Nothing here marks it ready.
            let native_mutations = Arc::new(koru_ai_server::NativeMutationGate::closed());
            let tools = Arc::new(koru_ai_server::AiToolDispatcher::new(
                koru_ai_server::StoreToolAdapter::new(store.clone()),
                koru_ai_server::ChatToolAdapter::new(root.path().join("ai")),
            ));
            // The mock registry avoids OS credentials; agent-codex still uses
            // the actual registered session adapter selected by the Bot.
            let config =
                koru_ai_server::WorkerConfig::deterministic_mock(root.path().join("ai"), segment())
                    .with_store_context(Arc::new(koru_ai_server::StoreContextAdapter::new(
                        store.clone(),
                    )))
                    .with_tool_executor(tools)
                    // Only catalog discovery is substituted; the delegate and
                    // gateway bridge below retain the actual sessions owner.
                    .with_session_host(catalog_endpoint)
                    .with_native_effect_recovery(store.clone(), native_mutations.clone())
                    .with_bot_authority(Arc::new(
                        koru_ai_server::BotAuthorityAdapter::from_device(
                            store,
                            &device.to_storage(),
                        )
                        .unwrap(),
                    ))
                    .with_bot_delegate(bot_port.clone());
            let ai = koru_ai_server::AiWorker::new(config).spawn().unwrap();
            let bridge = SessionBridge::connect_supervised(session_endpoint, offer(), None)
                .await
                .unwrap();
            let state = GatewayState::with_ai_events_and_services(
                segment().into(),
                worker,
                ai,
                Some(events),
                Some(bridge),
            );
            assert!(
                state.set_artifact_service(Arc::new(koru_artifact_api::Service::new(
                    koru_artifact_sqlite::Repository::open(&koru_channel::artifacts_root_beside(
                        &store_root
                    ))
                    .unwrap()
                )))
            );
            assert!(state.set_delegate_port(Arc::new(LocalDelegate::new(
                delegate_endpoint,
                "native-bot-fixture"
            ))));
            assert!(state.set_bot_delegate_port(bot_port));
            assert!(state.set_native_mutation_gate(native_mutations.clone()));
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            assert!(state.set_public_endpoint(address.clone()));
            let instance = uuid::Uuid::new_v4().to_string();
            let hello = greeting(
                &state,
                &address,
                &instance,
                &[
                    "bots.manage".into(),
                    "ai.chat".into(),
                    "store.read_documents".into(),
                    "store.edit_documents".into(),
                    "store.manage_workspaces".into(),
                    "events.read".into(),
                    "artifacts.read".into(),
                ],
                Admission::Vouched,
            );
            let ordinary = state.clone();
            let policy = Arc::new(move |request: &koru_api_discovery::ClientHello| {
                if let Some(refusal) = koru_api_discovery::admission_refusal(
                    request,
                    segment(),
                    koru_api_wire::API_VERSION,
                ) {
                    return refusal;
                }
                GatewayResponse::Welcome(Box::new(greeting(
                    &ordinary,
                    &address,
                    &instance,
                    &request.requested_scopes,
                    Admission::Ordinary,
                )))
            });
            let discover = tokio::spawn(rendezvous.serve(policy));
            let router = koru_api_gateway_core::router(state.clone());
            let serve = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            (
                state,
                vec![catalog_task, discover, serve],
                hello,
                catalog,
                native_mutations,
            )
        });
        let bootstrap = koru_delegate_host::LaunchBootstrap::new(
            uuid::Uuid::new_v4().to_string(),
            segment().into(),
            root.path().into(),
            store_root,
            true,
            device.to_storage(),
            uuid::Uuid::new_v4().to_string(),
        )
        .with_bot_authority(authority);
        let delegate_worker = Worker::start(root.path(), "delegate", &bootstrap, &path);
        let client = koru_api_client::Client::from_gateway(hello).unwrap();
        wait_until("scripted delegate owner", Duration::from_secs(90), || {
            LocalDelegate::new(
                koru_delegate_host::endpoint(segment(), Some(root.path())).unwrap(),
                "native-bot-fixture",
            )
            .serving()
        });
        // Observed, never forced: the AI worker's own GatewayStartup sweep
        // asks the delegate owner for pending native effects and opens the
        // gate only once that sweep completes with nothing left to settle.
        // Until the delegate serves, the sweep is deferred with backoff.
        let started = Instant::now();
        wait_until(
            "native startup recovery sweep",
            Duration::from_secs(120),
            || native_mutations.ready(),
        );
        eprintln!(
            "native mutation gate ready from startup sweep after {}ms",
            started.elapsed().as_millis()
        );
        Self {
            root,
            client,
            sessions,
            device,
            catalog,
            state,
            workers: vec![sessions_worker, delegate_worker],
            tasks,
            runtime,
            _slot: slot,
        }
    }
    pub fn store_root(&self) -> PathBuf {
        koru_channel::store_root_under(self.root.path())
    }
    pub fn verify_catalog(&self) {
        self.catalog.verify();
    }
    pub fn diagnose_attempt(&self, id: koru_ai_core::InferenceAttemptId) {
        self.catalog.diagnose();
        let Ok(ai) = koru_ai_sqlite::Repository::open(&self.root.path().join("ai/ai.db")) else {
            return;
        };
        let Ok(Some(receipt)) = ai.bot_submission_for_attempt(id) else {
            return;
        };
        let run = receipt.intent.submission.request.run;
        eprintln!("native fixture AI receipt stage: {:?}", receipt.stage);
        match receipt.outcome {
            Some(koru_ai_core::bots::BotExecutionOutcome::Accepted { admission, .. }) => {
                eprintln!("native fixture AI admission: {:?}", admission.state)
            }
            Some(
                koru_ai_core::bots::BotExecutionOutcome::Rejected { reason, .. }
                | koru_ai_core::bots::BotExecutionOutcome::RefusalFence { reason, .. },
            ) => eprintln!("native fixture AI refusal: {reason:?}"),
            None => eprintln!("native fixture AI outcome: pending"),
        }
        let runs = self.runs();
        use koru_delegate_sqlite::BotLedger as _;
        if let Ok(Some(execution)) = runs.bot_execution(run) {
            eprintln!("native fixture delegate admission: {:?}", execution.state);
        }
        if let Ok(attempt) = runs.current_attempt(run) {
            eprintln!(
                "native fixture execution: phase={:?} session_bound={}",
                attempt.as_ref().map(|row| row.phase),
                attempt
                    .and_then(|row| row.provider_session)
                    .and_then(|key| key.session)
                    .is_some()
            );
        }
        if let Ok(Some(intent)) = runs.intent(run) {
            eprintln!("native fixture intent: {:?}", intent.state);
            if let Some(document) = intent.run_document {
                if let Ok(snapshot) = self
                    .client
                    .store()
                    .snapshot(receipt.intent.submission.workspace, document)
                {
                    if let Ok(document) = RunDocument::from_snapshot(&snapshot) {
                        if let Ok(header) = document.header() {
                            if let RunStatus::Failed { stage } = header.status {
                                eprintln!("native fixture Run failure stage: {stage:?}");
                                eprintln!(
                                    "native fixture Run failure reason: {:?}",
                                    header.failure_reason
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    pub fn runs(&self) -> RunsDb {
        RunsDb::open(&koru_delegate_sqlite::runs_db_path(&self.store_root())).unwrap()
    }
}
fn greeting(
    state: &GatewayState,
    endpoint: &str,
    instance: &str,
    scopes: &[String],
    admission: Admission,
) -> GatewayHello {
    let (bearer, expires) =
        state
            .sessions
            .mint_admitted("native-bot-fixture", scopes, admission, now_millis());
    GatewayHello {
        api_version: koru_api_wire::API_VERSION,
        supervisor_launch_id: None,
        gateway_version: "fixture".into(),
        segment: segment().into(),
        api_endpoint: endpoint.into(),
        instance_id: instance.into(),
        capabilities: state.public_capabilities(),
        session: SessionCredential::new(bearer, expires),
    }
}
impl Drop for Stack {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.catalog.diagnose();
            // The scripted trace contains only fixed stage/method labels. Keep
            // it in the test log without retaining credentials or the root.
            if let Ok(trace) =
                std::fs::read_to_string(self.root.path().join("native-fixture.trace"))
            {
                for stage in trace
                    .lines()
                    .filter(|line| line.starts_with("native:") || line.starts_with("method:"))
                {
                    eprintln!("native fixture stage: {stage}");
                }
            }
        }
        while let Some(worker) = self.workers.pop() {
            drop(worker);
        }
        self.state.close_event_streams();
        self.state.stop_workers();
        self.state.join_ai();
        self.state.join_store();
        for task in &self.tasks {
            task.abort();
        }
        self.runtime
            .block_on(async { tokio::task::yield_now().await });
    }
}
