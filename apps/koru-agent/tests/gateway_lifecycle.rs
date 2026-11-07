// SPDX-License-Identifier: MPL-2.0

//! **All five steps, with a real session host on the other end of the bridge.**
//!
//! This is the one place the whole operation exists at once: a Store worker, a
//! supervised session host holding a launch-scoped lifecycle authority, a
//! terminal running inside a project scoped to a workspace, and a token the
//! Agent would have vouched for. Everything below is asserted through the
//! gateway's own router, so the ordering under test is the ordering a caller
//! gets.
//!
//! **The token is minted rather than obtained**, and that is not a shortcut
//! being taken — it is the mechanism working. A grant is minted only for a
//! process running the `koru` beside the Agent, and this test binary is not
//! that, so no test can ever hold one. What a test can do is arrange the state that
//! *follows* a successful admission, which is what `Admission::Vouched` is.
//!
//! **The session host is `koru-agent.exe --worker sessions`.** This file spawns
//! no gateway at all — it builds `GatewayState` in process and drives `router()`
//! with `tower::oneshot` — but it does need a real session host, and after the
//! standalone binaries were retired the only image that hosts that role is the
//! Agent's. That is also why the file lives in `apps/koru-agent`:
//! `CARGO_BIN_EXE_*` is set only for binaries the testing package declares.
//! There is no sibling to look for and nothing to skip on.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use koru_api_gateway_core::{
    Admission, GatewayState, MANAGE_WORKSPACES_SCOPE, SessionBridge, WORKSPACE_DESTROY_SCOPE,
    WORKSPACE_MIGRATE_SCOPE, now_millis,
};
use koru_session_protocol::{
    BuiltInProfileKey, CapabilityName, CapabilitySet, GridSize, MAX_PROTOCOL_VERSION,
    MIN_PROTOCOL_VERSION, ProcessState, ProfileKind, ProjectId, ProtocolOffer, ProtocolRange,
    SessionMode, SessionProfileKey, WorkspaceScope, capability,
};
use tower::ServiceExt as _;

/// A supervised session host on a root of its own, stopped when the test ends.
struct Host {
    child: Child,
    _bootstrap_pipe: std::process::ChildStdin,
    endpoint: koru_ipc::Endpoint,
    authority: String,
    lifecycle_authority: String,
}

impl Host {
    fn start(root: &Path) -> Self {
        std::fs::create_dir_all(root).unwrap();
        let launch_id = uuid::Uuid::new_v4().to_string();
        let authority = uuid::Uuid::new_v4().to_string();
        let lifecycle_authority = uuid::Uuid::new_v4().to_string();
        let bootstrap = koru_session_wire::LaunchBootstrap::new(
            launch_id.clone(),
            koru_channel::segment().into(),
            root.to_path_buf(),
            root.join("agent/sessions"),
            root.join("store"),
            true,
            authority.clone(),
        )
        .with_lifecycle_authority(lifecycle_authority.clone());
        let mut child = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_koru-agent")))
            .args(["--worker", "sessions", "--root"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(&serde_json::to_vec(&bootstrap).unwrap())
            .unwrap();
        pipe.write_all(b"\n").unwrap();
        pipe.flush().unwrap();
        let endpoint = koru_ipc::Endpoint::for_service_channel_root(
            koru_ipc::SESSION_HOST,
            koru_channel::segment(),
            root,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(client) = client(&endpoint) {
                assert_eq!(client.health().unwrap().supervisor_launch_id, launch_id);
                break;
            }
            assert!(child.try_wait().unwrap().is_none(), "the host exited");
            assert!(Instant::now() < deadline, "the host did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            child,
            _bootstrap_pipe: pipe,
            endpoint,
            authority,
            lifecycle_authority,
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Ok(client) = client(&self.endpoint) {
            let _ = client.stop_host(self.authority.clone());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn offer() -> ProtocolOffer {
    use std::str::FromStr as _;
    ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new(
            [
                capability::CONTROLLER_LEASES,
                capability::TERMINATE,
                capability::PROFILE_LAUNCH,
                capability::PROJECTS,
                capability::WORKSPACE_LIFECYCLE,
            ]
            .into_iter()
            .map(|name| CapabilityName::from_str(name).unwrap()),
        ),
    }
}

fn client(
    endpoint: &koru_ipc::Endpoint,
) -> Result<koru_session_client::SessionClient, koru_session_client::ClientError> {
    koru_session_client::SessionClient::connect(endpoint, "koru-lifecycle-gate", offer())
}

fn long_running(root: &Path, marker: &str) -> koru_session_wire::CreateSession {
    use std::str::FromStr as _;
    #[cfg(windows)]
    let argv = vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        "Start-Sleep -Seconds 60".into(),
    ];
    #[cfg(unix)]
    let argv = vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()];
    koru_session_wire::CreateSession {
        profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap()),
        parent_session_id: None,
        profile_kind: ProfileKind::Shell,
        display_name: marker.into(),
        working_directory: root.to_path_buf(),
        mode: SessionMode::RawPty,
        attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy::Unsupported,
        capabilities: CapabilitySet::default(),
        project_id: None,
        argv,
        environment: koru_session_wire::EnvironmentSpec::Inherit,
        size: GridSize::new(80, 24).unwrap(),
    }
}

struct Gateway {
    state: Arc<GatewayState>,
    token: String,
}

async fn gateway(root: &Path, host: &Host) -> Gateway {
    let bridge = SessionBridge::connect_supervised(
        host.endpoint.clone(),
        offer(),
        Some(host.lifecycle_authority.clone()),
    )
    .await
    .expect("the gateway's private bridge");
    let worker = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
        root.join("store"),
        koru_channel::segment(),
    ))
    .spawn()
    .unwrap();
    let state =
        GatewayState::new_with_services(koru_channel::segment().into(), worker, None, Some(bridge));
    let (token, _) = state.sessions.mint_admitted(
        "koru",
        &[
            WORKSPACE_DESTROY_SCOPE.into(),
            // The two vouched scopes are separate permissions, and the matrix
            // that proves it lives in `koru-api-gateway-core`. Here one token
            // holds both, because what is under test is the operations rather
            // than who may reach them.
            WORKSPACE_MIGRATE_SCOPE.into(),
            MANAGE_WORKSPACES_SCOPE.into(),
            "store.read_documents".into(),
        ],
        Admission::Vouched,
        now_millis(),
    );
    Gateway { state, token }
}

impl Gateway {
    async fn call(
        &self,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = koru_api_gateway_core::router(Arc::clone(&self.state))
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn workspace(&self, profile: &serde_json::Value, name: &str) -> serde_json::Value {
        let (status, workspace) = self
            .call(
                "POST",
                "/v1/workspaces",
                Some(serde_json::json!({ "owner": profile["id"], "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        workspace
    }

    async fn profile(&self) -> serde_json::Value {
        self.named_profile("Ada").await
    }

    async fn named_profile(&self, name: &str) -> serde_json::Value {
        let (status, profile) = self
            .call(
                "POST",
                "/v1/profiles",
                Some(serde_json::json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        profile
    }

    /// **Ask what the action would close, and answer with the token that came
    /// back.**
    ///
    /// The round trip a surface makes, with the reading and the clicking left
    /// out. It is a helper rather than a literal because the token is the
    /// gateway's: a body carrying a confirmation nobody issued is refused
    /// before a terminal is looked at, which is the whole point of the
    /// server-issued acceptance — and which is why this file's bodies could
    /// not stay hand-written UUIDs.
    async fn lifecycle_body(&self, workspace: &str, action: &str) -> serde_json::Value {
        let (status, consequence) = self
            .call(
                "POST",
                &format!("/v1/workspaces/{workspace}/lifecycle/confirmation"),
                Some(serde_json::json!({ "action": action })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{consequence}");
        serde_json::json!({
            "action": action,
            "confirmation": consequence["confirmation"],
        })
    }

    /// The same round trip for a move. The token is bound to the destination
    /// as well as the workspace, so the two travel together.
    async fn migration_body(&self, workspace: &str, to: &str) -> serde_json::Value {
        let (status, consequence) = self
            .call(
                "POST",
                &format!("/v1/workspaces/{workspace}/migration/confirmation"),
                Some(serde_json::json!({ "to": to })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{consequence}");
        serde_json::json!({
            "to": to,
            "confirmation": consequence["confirmation"],
        })
    }
}

fn scope(profile: &serde_json::Value, workspace: &serde_json::Value) -> WorkspaceScope {
    WorkspaceScope {
        profile: profile["id"].as_str().unwrap().parse().unwrap(),
        workspace: workspace["id"].as_str().unwrap().parse().unwrap(),
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not settle");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// **The whole contract, once through and then again.**
///
/// A workspace with a live terminal in a project scoped to it. Soft-deleting it
/// closes that terminal, commits the mutation, and says both — and the terminal
/// belonging to the *other* workspace is untouched, which is the join being
/// right rather than the operation being enthusiastic. Then the same request
/// again: nothing left to close, the mutation is already what it should be, and
/// the answer is success with an empty list.
#[tokio::test(flavor = "multi_thread")]
async fn soft_deleting_a_workspace_closes_its_terminals_then_commits_and_is_retryable() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let gateway = gateway(root.path(), &host).await;
    let profile = gateway.profile().await;
    let doomed = gateway.workspace(&profile, "Doomed").await;
    let spared = gateway.workspace(&profile, "Spared").await;

    let terminals = client(&host.endpoint).unwrap();
    let doomed_project = ProjectId::mint();
    let spared_project = ProjectId::mint();
    terminals
        .create_project(
            doomed_project,
            "Doomed work",
            None,
            Some(scope(&profile, &doomed)),
        )
        .unwrap();
    terminals
        .create_project(
            spared_project,
            "Spared work",
            None,
            Some(scope(&profile, &spared)),
        )
        .unwrap();
    let mut inside = long_running(root.path(), "KORU:DOOMED");
    inside.project_id = Some(doomed_project);
    let inside = terminals.create(inside).unwrap();
    let mut elsewhere = long_running(root.path(), "KORU:SPARED");
    elsewhere.project_id = Some(spared_project);
    let elsewhere = terminals.create(elsewhere).unwrap();

    let (status, answer) = gateway
        .call(
            "POST",
            &format!(
                "/v1/workspaces/{}/lifecycle",
                doomed["id"].as_str().unwrap()
            ),
            Some(
                gateway
                    .lifecycle_body(doomed["id"].as_str().unwrap(), "soft_delete")
                    .await,
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        answer["terminated_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![inside.id.to_string()],
        "the operation closed something other than the workspace's own terminal"
    );
    assert_eq!(answer["sessions_closed"], serde_json::Value::Bool(true));
    assert_eq!(answer["mutation_applied"], serde_json::Value::Bool(true));

    // Step 4 committed: the workspace is in the trash, and the ordinary listing
    // no longer names it.
    let (_, listed) = gateway.call("GET", "/v1/workspaces", None).await;
    assert!(
        !listed
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == doomed["id"])
    );

    // Step 3 really closed it, and closed nothing else.
    wait_until(|| {
        terminals
            .list()
            .unwrap()
            .iter()
            .all(|session| session.id != inside.id || session.process != ProcessState::Running)
    });
    assert_eq!(
        terminals
            .list()
            .unwrap()
            .iter()
            .find(|session| session.id == elsewhere.id)
            .map(|session| session.process.clone()),
        Some(ProcessState::Running),
        "another workspace's terminal was closed"
    );

    // **The retry.** Idempotent on both halves: nothing left to close, and a
    // soft delete of an already-deleted workspace is the outcome the caller
    // wanted rather than a failure.
    let (status, again) = gateway
        .call(
            "POST",
            &format!(
                "/v1/workspaces/{}/lifecycle",
                doomed["id"].as_str().unwrap()
            ),
            Some(
                gateway
                    .lifecycle_body(doomed["id"].as_str().unwrap(), "soft_delete")
                    .await,
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert!(again["terminated_sessions"].as_array().unwrap().is_empty());
    assert_eq!(again["mutation_applied"], serde_json::Value::Bool(true));

    // Restore brings it back and closes nothing.
    let (status, restored) = gateway
        .call(
            "POST",
            &format!(
                "/v1/workspaces/{}/lifecycle",
                doomed["id"].as_str().unwrap()
            ),
            Some(
                gateway
                    .lifecycle_body(doomed["id"].as_str().unwrap(), "restore")
                    .await,
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(
        restored["terminated_sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    terminals.disconnect();
    drop(host);
}

/// **S24, arranged rather than described.**
///
/// Another window soft-deletes the workspace between the confirmation and the
/// commit. The operation still closes the terminals — that is step 3, and it
/// runs before anything is applied — and then the archive fails, because a
/// workspace in the trash has no appearance to change. The answer is exactly
/// that truth: the terminals are closed, the catalog is unchanged, the
/// workspace is where it was, and a retry is offered.
#[tokio::test(flavor = "multi_thread")]
async fn closed_terminals_and_an_uncommitted_mutation_report_exactly_that() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let gateway = gateway(root.path(), &host).await;
    let profile = gateway.profile().await;
    let workspace = gateway.workspace(&profile, "Contested").await;
    let id = workspace["id"].as_str().unwrap().to_owned();

    let terminals = client(&host.endpoint).unwrap();
    let project = ProjectId::mint();
    terminals
        .create_project(project, "Work", None, Some(scope(&profile, &workspace)))
        .unwrap();
    let mut running = long_running(root.path(), "KORU:CONTESTED");
    running.project_id = Some(project);
    let running = terminals.create(running).unwrap();

    // The other window gets there first, through the Store's own route.
    let (status, _) = gateway
        .call("DELETE", &format!("/v1/workspaces/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, answer) = gateway
        .call(
            "POST",
            &format!("/v1/workspaces/{id}/lifecycle"),
            Some(gateway.lifecycle_body(&id, "archive").await),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the mutation's own refusal is what comes back: {answer}"
    );
    assert_eq!(answer["code"], "workspace_lifecycle_incomplete");
    assert_eq!(answer["retryable"], serde_json::Value::Bool(true));
    assert_eq!(
        answer["outcome"]["sessions_closed"],
        serde_json::Value::Bool(true),
        "the terminals were closed and the answer did not say so: {answer}"
    );
    assert_eq!(
        answer["outcome"]["mutation_applied"],
        serde_json::Value::Bool(false)
    );
    assert_eq!(
        answer["outcome"]["terminated_sessions"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{answer}"
    );

    // And it is true: the terminal is gone and the workspace is exactly where
    // the other window left it.
    wait_until(|| {
        terminals
            .list()
            .unwrap()
            .iter()
            .all(|session| session.id != running.id || session.process != ProcessState::Running)
    });
    let (_, listed) = gateway
        .call("GET", "/v1/workspaces?include_deleted=true", None)
        .await;
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == workspace["id"])
        .expect("the workspace is still there");
    assert!(!row["deleted_at"].is_null(), "the trash was emptied");
    assert_eq!(
        row["archived"],
        serde_json::Value::Bool(false),
        "the mutation was reported as uncommitted and committed anyway"
    );

    // The retry is the same request, and it tells the same truth — nothing left
    // to close, and the mutation still refused.
    let (status, again) = gateway
        .call(
            "POST",
            &format!("/v1/workspaces/{id}/lifecycle"),
            Some(gateway.lifecycle_body(&id, "archive").await),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        again["outcome"]["terminated_sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        again["outcome"]["mutation_applied"],
        serde_json::Value::Bool(false)
    );

    terminals.disconnect();
    drop(host);
}

/// **A workspace changes hands, and its projects follow it.**
///
/// All six steps at once, with a real host: the terminal running in a project
/// scoped to the workspace is closed, the Store commits the new owner, and the
/// project's typed scope is repaired so the terminal's tree does not have to
/// pretend. The project in the *other* workspace is untouched, which is the
/// join being right rather than the repair being enthusiastic.
#[tokio::test(flavor = "multi_thread")]
async fn moving_a_workspace_closes_its_terminals_commits_and_rescopes_its_projects() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let gateway = gateway(root.path(), &host).await;
    let from = gateway.named_profile("Work").await;
    let to = gateway.named_profile("Home").await;
    let travelling = gateway.workspace(&from, "Travelling").await;
    let staying = gateway.workspace(&from, "Staying").await;
    let id = travelling["id"].as_str().unwrap().to_owned();

    let terminals = client(&host.endpoint).unwrap();
    let moving_project = ProjectId::mint();
    let staying_project = ProjectId::mint();
    terminals
        .create_project(
            moving_project,
            "Moving work",
            None,
            Some(scope(&from, &travelling)),
        )
        .unwrap();
    terminals
        .create_project(
            staying_project,
            "Staying work",
            None,
            Some(scope(&from, &staying)),
        )
        .unwrap();
    let mut inside = long_running(root.path(), "KORU:MOVING");
    inside.project_id = Some(moving_project);
    let inside = terminals.create(inside).unwrap();
    let mut beside = long_running(root.path(), "KORU:STAYING");
    beside.project_id = Some(staying_project);
    let beside = terminals.create(beside).unwrap();

    let body = gateway
        .migration_body(&id, to["id"].as_str().unwrap())
        .await;
    let (status, answer) = gateway
        .call(
            "POST",
            &format!("/v1/workspaces/{id}/migration"),
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["from"], from["id"]);
    assert_eq!(answer["to"], to["id"]);
    assert_eq!(
        answer["terminated_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![inside.id.to_string()],
        "the move closed something other than the workspace's own terminal"
    );
    assert_eq!(answer["mutation_applied"], serde_json::Value::Bool(true));
    assert_eq!(
        answer["projects_rescoped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![moving_project.to_string()],
        "the projects did not follow the workspace: {answer}"
    );

    // The Store committed, and nothing about the workspace changed on the way.
    let (_, row) = gateway
        .call("GET", &format!("/v1/workspaces/{id}"), None)
        .await;
    assert_eq!(row["owner"], to["id"]);
    assert_eq!(row["id"], travelling["id"]);
    assert_eq!(row["name"], travelling["name"]);

    // The project names the new owner and the same workspace; its neighbour is
    // exactly where it was.
    let projects = terminals.list_projects().unwrap();
    let scoped = |id| {
        projects
            .iter()
            .find(|project| project.id == id)
            .expect("a project vanished")
            .workspace_scope
    };
    assert_eq!(scoped(moving_project), Some(scope(&to, &travelling)));
    assert_eq!(scoped(staying_project), Some(scope(&from, &staying)));

    // Step 3 really closed it, and closed nothing else.
    wait_until(|| {
        terminals
            .list()
            .unwrap()
            .iter()
            .all(|session| session.id != inside.id || session.process != ProcessState::Running)
    });
    assert_eq!(
        terminals
            .list()
            .unwrap()
            .iter()
            .find(|session| session.id == beside.id)
            .map(|session| session.process.clone()),
        Some(ProcessState::Running),
        "another workspace's terminal was closed by a move"
    );

    // **The retry, which is where the repair earns its shape.** The workspace
    // is already where it was sent, so nothing is closed and nothing is
    // written — and the rescope still runs, because that is the branch a
    // migration whose Store write landed while the host was unreachable comes
    // back through. It finds everything already right and says so.
    let body = gateway
        .migration_body(&id, to["id"].as_str().unwrap())
        .await;
    let (status, again) = gateway
        .call(
            "POST",
            &format!("/v1/workspaces/{id}/migration"),
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert!(again["terminated_sessions"].as_array().unwrap().is_empty());
    assert_eq!(again["mutation_applied"], serde_json::Value::Bool(true));
    assert!(
        again["projects_rescoped"].as_array().unwrap().is_empty(),
        "a repair that changed nothing reported something: {again}"
    );

    terminals.disconnect();
    drop(host);
}
