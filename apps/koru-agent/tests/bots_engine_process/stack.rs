// SPDX-License-Identifier: MPL-2.0
use koru_api_gateway_core::{GatewayState, bots_workspace::Engine};
use koru_bots_engine::{authority::Secret, person, worker, workspace};
use koru_store_core::{Locale, Timestamp, WorkspaceId};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

pub fn until(label: &str, timeout: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(250));
    }
}
pub struct Stack {
    http: reqwest::blocking::Client,
    pub root: PathBuf,
    pub workspace: WorkspaceId,
    pub role: Uuid,
    pub endpoint: String,
    pub bearer: String,
    owner: String,
    engine: Engine,
    child: Child,
    launch: Command,
    bootstrap: worker::Bootstrap,
    pipe: Option<ChildStdin>,
    state: Arc<GatewayState>,
    runtime: tokio::runtime::Runtime,
    stop: Option<oneshot::Sender<()>>,
    service: Option<JoinHandle<()>>,
    http_stop: Option<oneshot::Sender<()>>,
    http_service: Option<JoinHandle<()>>,
}
impl Stack {
    pub fn start(root: &Path, real: Option<&Path>) -> Self {
        let bin = root.join("fixture-bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::hard_link(
            std::env::current_exe().unwrap(),
            bin.join(if real.is_some() {
                "claude-agent-acp.exe"
            } else {
                "codex.exe"
            }),
        )
        .unwrap();
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))
                .unwrap();
        let bootstrap = worker::Bootstrap {
            launch_id: Uuid::new_v4(),
            segment: koru_channel::segment().into(),
            root: root.into(),
            authority: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        };
        let engine = Engine {
            endpoint: worker::endpoint(root, koru_channel::segment()).unwrap(),
            authority: Secret::parse(bootstrap.authority.clone()).unwrap(),
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-agent"));
        command
            .args(["--worker", "bots", "--root"])
            .arg(root)
            .env("PATH", path)
            .env("BOTS_ENGINE_FIXTURE_TRACE", root.join("provider.trace"))
            .env("BOTS_ENGINE_FIXTURE_ROOT", root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(root.join("engine.log")).unwrap());
        if let Some(entry) = real {
            command.env("BOTS_ENGINE_FIXTURE_PROVIDER_ENTRY", entry);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        serde_json::to_writer(&mut pipe, &bootstrap).unwrap();
        pipe.write_all(b"\n").unwrap();
        pipe.flush().unwrap();
        until("engine ready", Duration::from_secs(30), || {
            matches!(
                worker::request(
                    &engine.endpoint,
                    engine.authority.expose(),
                    worker::Command::Hello
                ),
                Ok(worker::Reply::Hello { .. })
            )
        });
        let store = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
            koru_channel::store_root_under(root),
            koru_channel::segment(),
        ))
        .spawn()
        .unwrap();
        let (owner, workspace, role) = store
            .client()
            .with_store(|store| {
                let at = Timestamp::now();
                let profile =
                    store.create_profile("Synthetic", None, None, Locale::default(), at)?;
                let workspace = store.create_workspace(profile.id, "Synthetic", at)?.id;
                let mut page = koru_core::WorkspaceDoc::create(
                    koru_core::WorkspaceId::new(),
                    "Synthetic Role",
                )
                .unwrap();
                let document = page
                    .insert(
                        koru_core::Parent::Root,
                        koru_core::Node::document("Synthetic Role").unwrap(),
                    )
                    .unwrap();
                page.insert(
                    koru_core::Parent::Node(document),
                    koru_core::Node::paragraph("Perform only the synthetic fixture task.").unwrap(),
                )
                .unwrap();
                let (role, _) = store.import_document(
                    workspace,
                    "Synthetic Role",
                    "page",
                    None,
                    page.export_snapshot().unwrap(),
                    at,
                )?;
                Ok::<_, koru_store_core::Error>((
                    store.local_user_scope()?.to_storage(),
                    workspace,
                    role.as_uuid(),
                ))
            })
            .unwrap()
            .unwrap();
        let ai = koru_ai_server::AiWorker::new(
            koru_ai_server::WorkerConfig::deterministic_mock(
                root.join("ai"),
                koru_channel::segment(),
            )
            .with_store_context(Arc::new(koru_ai_server::StoreContextAdapter::new(
                store.client(),
            ))),
        )
        .spawn()
        .unwrap();
        let state = GatewayState::with_ai(koru_channel::segment().into(), store, ai);
        assert!(state.set_bots_engine(engine.clone(), &Uuid::new_v4().to_string(), root.into()));
        let boundary = Arc::new(state.bots_workspace(engine.clone()));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let endpoint = workspace::endpoint(root, koru_channel::segment()).unwrap();
        let (stop, stopping) = oneshot::channel();
        let service = runtime.block_on(async {
            let listener = koru_ipc::AsyncListener::bind(endpoint).unwrap();
            tokio::spawn(serve(listener, boundary, stopping))
        });
        let (http_stop, http_stopping) = oneshot::channel();
        let (endpoint, http_service) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            assert!(state.set_public_endpoint(endpoint.clone()));
            let router = koru_api_gateway_core::router(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(async {
                        let _ = http_stopping.await;
                    })
                    .await
                    .unwrap();
            });
            (endpoint, task)
        });
        let (bearer, _) = state.sessions.mint_admitted(
            "synthetic engine proof",
            &["bots.manage".into(), "ai.chat".into()],
            koru_api_gateway_core::Admission::Vouched,
            koru_api_gateway_core::now_millis(),
        );
        Self {
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            root: root.into(),
            launch: command,
            bootstrap,
            workspace,
            role,
            endpoint,
            bearer,
            owner,
            engine,
            child,
            pipe: Some(pipe),
            state,
            runtime,
            stop: Some(stop),
            service: Some(service),
            http_stop: Some(http_stop),
            http_service: Some(http_service),
        }
    }
    pub fn person(&self, command: person::Command) -> Value {
        match worker::request(
            &self.engine.endpoint,
            self.engine.authority.expose(),
            worker::Command::Person(person::Request {
                owner: self.owner.clone(),
                sender: format!("person:{}", self.owner),
                command,
            }),
        )
        .unwrap()
        {
            worker::Reply::Person(reply) => reply.0,
            other => panic!("person command refused: {other:?}"),
        }
    }
    pub fn finish(mut self) {
        self.shutdown();
    }
    pub fn restart_after_write(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.pipe.take();
        std::fs::write(self.root.join("resume.allowed"), b"resume").unwrap();
        self.bootstrap.launch_id = Uuid::new_v4();
        self.child = self.launch.spawn().unwrap();
        let mut pipe = self.child.stdin.take().unwrap();
        serde_json::to_writer(&mut pipe, &self.bootstrap).unwrap();
        pipe.write_all(b"\n").unwrap();
        pipe.flush().unwrap();
        self.pipe = Some(pipe);
        until(
            "replacement engine ready",
            Duration::from_secs(30),
            || matches!(worker::request(&self.engine.endpoint, self.engine.authority.expose(), worker::Command::Hello), Ok(worker::Reply::Hello { launch_id }) if launch_id == self.bootstrap.launch_id),
        );
    }
    pub fn status(&self) -> koru_bots_engine::Status {
        match worker::request(
            &self.engine.endpoint,
            self.engine.authority.expose(),
            worker::Command::Status,
        )
        .unwrap()
        {
            worker::Reply::Status(status) => status,
            other => panic!("status refused: {other:?}"),
        }
    }
    pub fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }
    pub fn send(&self, envelope: koru_bots_engine::messages::Envelope) -> Value {
        self.thread(koru_ai_wire::bot_threads::Command::Send {
            id: envelope.id,
            bot: koru_ai_core::bots::BotId::from_uuid(envelope.to),
            thread: koru_ai_core::ChatId::from_uuid(envelope.thread.unwrap()),
            body: envelope.body,
            in_reply_to: envelope.in_reply_to,
            attachments: envelope.attachments,
            references: envelope.references,
        })
    }
    pub fn thread(&self, command: koru_ai_wire::bot_threads::Command) -> Value {
        let response = self
            .http()
            .post(format!("{}/v1/ai/bots/threads", self.endpoint))
            .bearer_auth(&self.bearer)
            .json(&command)
            .send()
            .unwrap();
        let status = response.status();
        let value: Value = response.json().unwrap();
        assert!(
            status.is_success(),
            "thread route refused: {}",
            value["code"]
        );
        value
    }
    fn shutdown(&mut self) {
        if self.pipe.take().is_none() {
            return;
        }
        until("engine drains workers", Duration::from_secs(30), || {
            self.child.try_wait().unwrap().is_some()
        });
        self.stop_http();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(service) = self.service.take() {
            self.runtime.block_on(service).unwrap();
        }
        self.state.stop_workers();
        self.state.join_ai();
        assert!(self.state.join_store().is_some());
    }
    fn stop_http(&mut self) {
        self.state.close_event_streams();
        if let Some(stop) = self.http_stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.http_service.take() {
            let _ = self.runtime.block_on(task);
        }
    }
}
impl Drop for Stack {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.pipe.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.stop_http();
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(service) = self.service.take() {
                let _ = self.runtime.block_on(service);
            }
            self.state.stop_workers();
            self.state.join_ai();
            self.state.join_store();
        } else {
            self.shutdown();
        }
        // Keep databases and observations, not a hard link retaining every
        // superseded debug executable across later harness builds.
        for name in ["codex.exe", "claude-agent-acp.exe"] {
            let _ = std::fs::remove_file(self.root.join("fixture-bin").join(name));
        }
    }
}
async fn serve(
    listener: koru_ipc::AsyncListener,
    boundary: Arc<koru_api_gateway_core::bots_workspace::Boundary>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut calls = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = calls.join_next(), if !calls.is_empty() => {},
            stream = listener.accept(), if calls.len() < 8 => {
                let Ok(mut stream) = stream else { break; };
                let boundary = boundary.clone();
                calls.spawn(async move {
                    let mut header = [0; koru_ipc::frame::HEADER_LEN];
                    stream.read_exact(&mut header).await.unwrap();
                    let header = koru_ipc::frame::parse_header(&header).unwrap();
                    assert_eq!(header.version, workspace::VERSION);
                    let mut bytes = vec![0; header.body_len];
                    stream.read_exact(&mut bytes).await.unwrap();
                    let request = koru_ipc::frame::decode(&bytes).unwrap();
                    let reply = tokio::task::spawn_blocking(move || boundary.handle(request)).await.unwrap();
                    let bytes = koru_ipc::frame::encode(workspace::VERSION, &reply).unwrap();
                    stream.write_all(&bytes).await.unwrap();
                });
            }
        }
    }
    while calls.join_next().await.is_some() {}
}
